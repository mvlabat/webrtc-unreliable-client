
// client implements the API for a TURN client
use super::binding::*;
use super::periodic_timer::*;
use super::permission::*;
use super::transaction::*;
use crate::webrtc::turn::proto;
use crate::webrtc::turn::Error;

use crate::webrtc::stun::agent::*;
use crate::webrtc::stun::attributes::*;
use crate::webrtc::stun::error_code::*;
use crate::webrtc::stun::fingerprint::*;
use crate::webrtc::stun::integrity::*;
use crate::webrtc::stun::message::*;
use crate::webrtc::stun::textattrs::*;

use crate::webrtc::util::Conn;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{Duration, Instant};

use async_trait::async_trait;

const MAX_RETRY_ATTEMPTS: u16 = 3;

pub(crate) struct InboundData {
    pub(crate) data: Vec<u8>,
    pub(crate) from: SocketAddr,
}

// UDPConnObserver is an interface to UDPConn observer
#[async_trait]
pub(crate) trait RelayConnObserver {
    fn turn_server_addr(&self) -> String;
    fn username(&self) -> Username;
    fn realm(&self) -> Realm;
    async fn write_to(&self, data: &[u8], to: &str) -> Result<usize, crate::webrtc::util::Error>;
    async fn perform_transaction(
        &mut self,
        msg: &Message,
        to: &str,
        ignore_result: bool,
    ) -> Result<TransactionResult, Error>;
}

pub(crate) struct RelayConnInternal<T: 'static + RelayConnObserver + Send + Sync> {
    obs: Arc<Mutex<T>>,
    perm_map: PermissionMap,
    binding_mgr: Arc<Mutex<BindingManager>>,
    integrity: MessageIntegrity,
    nonce: Nonce,
    lifetime: Duration,
}

// RelayConn is the implementation of the Conn interfaces for UDP Relayed network connections.
pub(crate) struct RelayConn<T: 'static + RelayConnObserver + Send + Sync> {
    relayed_addr: SocketAddr,
    read_ch_rx: Arc<Mutex<mpsc::Receiver<InboundData>>>,
    relay_conn: Arc<Mutex<RelayConnInternal<T>>>,
    refresh_alloc_timer: PeriodicTimer,
    refresh_perms_timer: PeriodicTimer,
}

#[async_trait]
impl<T: RelayConnObserver + Send + Sync> Conn for RelayConn<T> {
    async fn connect(&self, _addr: SocketAddr) -> Result<(), crate::webrtc::util::Error> {
        Err(io::Error::new(io::ErrorKind::Other, "Not applicable").into())
    }

    async fn recv(&self, _buf: &mut [u8]) -> Result<usize, crate::webrtc::util::Error> {
        Err(io::Error::new(io::ErrorKind::Other, "Not applicable").into())
    }

    // ReadFrom reads a packet from the connection,
    // copying the payload into p. It returns the number of
    // bytes copied into p and the return address that
    // was on the packet.
    // It returns the number of bytes read (0 <= n <= len(p))
    // and any error encountered. Callers should always process
    // the n > 0 bytes returned before considering the error err.
    // ReadFrom can be made to time out and return
    // an Error with Timeout() == true after a fixed time limit;
    // see SetDeadline and SetReadDeadline.
    async fn recv_from(&self, p: &mut [u8]) -> Result<(usize, SocketAddr), crate::webrtc::util::Error> {
        let mut read_ch_rx = self.read_ch_rx.lock().await;

        if let Some(ib_data) = read_ch_rx.recv().await {
            let n = ib_data.data.len();
            if p.len() < n {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    Error::ErrShortBuffer.to_string(),
                )
                .into());
            }
            p[..n].copy_from_slice(&ib_data.data);
            Ok((n, ib_data.from))
        } else {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                Error::ErrAlreadyClosed.to_string(),
            )
            .into())
        }
    }

    async fn send(&self, _buf: &[u8]) -> Result<usize, crate::webrtc::util::Error> {
        Err(io::Error::new(io::ErrorKind::Other, "Not applicable").into())
    }

    // write_to writes a packet with payload p to addr.
    // write_to can be made to time out and return
    // an Error with Timeout() == true after a fixed time limit;
    // see SetDeadline and SetWriteDeadline.
    // On packet-oriented connections, write timeouts are rare.
    async fn send_to(&self, p: &[u8], addr: SocketAddr) -> Result<usize, crate::webrtc::util::Error> {
        let mut relay_conn = self.relay_conn.lock().await;
        match relay_conn.send_to(p, addr).await {
            Ok(n) => Ok(n),
            Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string()).into()),
        }
    }

    // LocalAddr returns the local network address.
    async fn local_addr(&self) -> Result<SocketAddr, crate::webrtc::util::Error> {
        Ok(self.relayed_addr)
    }

    async fn remote_addr(&self) -> Option<SocketAddr> {
        None
    }

    // Close closes the connection.
    // Any blocked ReadFrom or write_to operations will be unblocked and return errors.
    async fn close(&self) -> Result<(), crate::webrtc::util::Error> {
        self.refresh_alloc_timer.stop().await;
        self.refresh_perms_timer.stop().await;

        let mut relay_conn = self.relay_conn.lock().await;
        let _ = relay_conn
            .close()
            .await
            .map_err(|err| crate::webrtc::util::Error::Other(format!("{}", err)));
        Ok(())
    }
}

impl<T: RelayConnObserver + Send + Sync> RelayConnInternal<T> {

    // write_to writes a packet with payload p to addr.
    // write_to can be made to time out and return
    // an Error with Timeout() == true after a fixed time limit;
    // see SetDeadline and SetWriteDeadline.
    // On packet-oriented connections, write timeouts are rare.
    async fn send_to(&mut self, p: &[u8], addr: SocketAddr) -> Result<usize, Error> {
        // check if we have a permission for the destination IP addr
        let perm = if let Some(perm) = self.perm_map.find(&addr) {
            Arc::clone(perm)
        } else {
            let perm = Arc::new(Permission::default());
            self.perm_map.insert(&addr, Arc::clone(&perm));
            perm
        };

        let mut result = Ok(());
        for _ in 0..MAX_RETRY_ATTEMPTS {
            result = self.create_perm(&perm, addr).await;
            if let Err(err) = &result {
                if Error::ErrTryAgain != *err {
                    break;
                }
            }
        }
        if let Err(err) = result {
            return Err(err);
        }

        let number = {
            let (bind_st, bind_at, bind_number, bind_addr) = {
                let mut binding_mgr = self.binding_mgr.lock().await;
                let b = if let Some(b) = binding_mgr.find_by_addr(&addr) {
                    b
                } else {
                    binding_mgr
                        .create(addr)
                        .ok_or_else(|| Error::Other("Addr not found".to_owned()))?
                };
                (b.state(), b.refreshed_at(), b.number, b.addr)
            };

            if bind_st == BindingState::Idle
                || bind_st == BindingState::Request
                || bind_st == BindingState::Failed
            {
                // block only callers with the same binding until
                // the binding transaction has been complete
                // binding state may have been changed while waiting. check again.
                if bind_st == BindingState::Idle {
                    let binding_mgr = Arc::clone(&self.binding_mgr);
                    let rc_obs = Arc::clone(&self.obs);
                    let nonce = self.nonce.clone();
                    let integrity = self.integrity.clone();
                    {
                        let mut bm = binding_mgr.lock().await;
                        if let Some(b) = bm.get_by_addr(&bind_addr) {
                            b.set_state(BindingState::Request);
                        }
                    }
                    tokio::spawn(async move {
                        let result = RelayConnInternal::bind(
                            rc_obs,
                            bind_addr,
                            bind_number,
                            nonce,
                            integrity,
                        )
                        .await;

                        {
                            let mut bm = binding_mgr.lock().await;
                            if let Err(err) = result {
                                if Error::ErrUnexpectedResponse != err {
                                    bm.delete_by_addr(&bind_addr);
                                } else if let Some(b) = bm.get_by_addr(&bind_addr) {
                                    b.set_state(BindingState::Failed);
                                }

                                // keep going...
                                log::warn!("bind() failed: {}", err);
                            } else if let Some(b) = bm.get_by_addr(&bind_addr) {
                                b.set_state(BindingState::Ready);
                            }
                        }
                    });
                }

                // send data using SendIndication
                let peer_addr = socket_addr2peer_address(&addr);
                let mut msg = Message::new();
                msg.build(&[
                    Box::new(TransactionId::new()),
                    Box::new(MessageType::new(METHOD_SEND, CLASS_INDICATION)),
                    Box::new(proto::data::Data(p.to_vec())),
                    Box::new(peer_addr),
                    Box::new(FINGERPRINT),
                ])?;

                // indication has no transaction (fire-and-forget)
                let obs = self.obs.lock().await;
                let turn_server_addr = obs.turn_server_addr();
                return Ok(obs.write_to(&msg.raw, &turn_server_addr).await?);
            }

            // binding is either ready

            // check if the binding needs a refresh
            if bind_st == BindingState::Ready
                && Instant::now()
                    .checked_duration_since(bind_at)
                    .unwrap_or_else(|| Duration::from_secs(0))
                    > Duration::from_secs(5 * 60)
            {
                let binding_mgr = Arc::clone(&self.binding_mgr);
                let rc_obs = Arc::clone(&self.obs);
                let nonce = self.nonce.clone();
                let integrity = self.integrity.clone();
                {
                    let mut bm = binding_mgr.lock().await;
                    if let Some(b) = bm.get_by_addr(&bind_addr) {
                        b.set_state(BindingState::Refresh);
                    }
                }
                tokio::spawn(async move {
                    let result =
                        RelayConnInternal::bind(rc_obs, bind_addr, bind_number, nonce, integrity)
                            .await;

                    {
                        let mut bm = binding_mgr.lock().await;
                        if let Err(err) = result {
                            if Error::ErrUnexpectedResponse != err {
                                bm.delete_by_addr(&bind_addr);
                            } else if let Some(b) = bm.get_by_addr(&bind_addr) {
                                b.set_state(BindingState::Failed);
                            }

                            // keep going...
                            log::warn!("bind() for refresh failed: {}", err);
                        } else if let Some(b) = bm.get_by_addr(&bind_addr) {
                            b.set_refreshed_at(Instant::now());
                            b.set_state(BindingState::Ready);
                        }
                    }
                });
            }

            bind_number
        };

        // send via ChannelData
        self.send_channel_data(p, number).await
    }

    // This func-block would block, per destination IP (, or perm), until
    // the perm state becomes "requested". Purpose of this is to guarantee
    // the order of packets (within the same perm).
    // Note that CreatePermission transaction may not be complete before
    // all the data transmission. This is done assuming that the request
    // will be mostly likely successful and we can tolerate some loss of
    // UDP packet (or reorder), inorder to minimize the latency in most cases.
    async fn create_perm(&mut self, perm: &Arc<Permission>, addr: SocketAddr) -> Result<(), Error> {
        if perm.state() == PermState::Idle {
            // punch a hole! (this would block a bit..)
            if let Err(err) = self.create_permissions(&[addr]).await {
                self.perm_map.delete(&addr);
                return Err(err);
            }
            perm.set_state(PermState::Permitted);
        }
        Ok(())
    }

    async fn send_channel_data(&self, data: &[u8], ch_num: u16) -> Result<usize, Error> {
        let mut ch_data = proto::chandata::ChannelData {
            data: data.to_vec(),
            number: proto::channum::ChannelNumber(ch_num),
            ..Default::default()
        };
        ch_data.encode();

        let obs = self.obs.lock().await;
        Ok(obs.write_to(&ch_data.raw, &obs.turn_server_addr()).await?)
    }

    async fn create_permissions(&mut self, addrs: &[SocketAddr]) -> Result<(), Error> {
        let res = {
            let msg = {
                let obs = self.obs.lock().await;
                let mut setters: Vec<Box<dyn Setter>> = vec![
                    Box::new(TransactionId::new()),
                    Box::new(MessageType::new(METHOD_CREATE_PERMISSION, CLASS_REQUEST)),
                ];

                for addr in addrs {
                    setters.push(Box::new(socket_addr2peer_address(addr)));
                }

                setters.push(Box::new(obs.username()));
                setters.push(Box::new(obs.realm()));
                setters.push(Box::new(self.nonce.clone()));
                setters.push(Box::new(self.integrity.clone()));
                setters.push(Box::new(FINGERPRINT));

                let mut msg = Message::new();
                msg.build(&setters)?;
                msg
            };

            let mut obs = self.obs.lock().await;
            let turn_server_addr = obs.turn_server_addr();

            log::debug!("UDPConn.createPermissions call PerformTransaction 1");
            let tr_res = obs
                .perform_transaction(&msg, &turn_server_addr, false)
                .await?;

            tr_res.msg
        };

        if res.typ.class == CLASS_ERROR_RESPONSE {
            let mut code = ErrorCodeAttribute::default();
            let result = code.get_from(&res);
            if result.is_err() {
                return Err(Error::Other(format!("{}", res.typ)));
            } else if code.code == CODE_STALE_NONCE {
                self.set_nonce_from_msg(&res);
                return Err(Error::ErrTryAgain);
            } else {
                return Err(Error::Other(format!("{} (error {})", res.typ, code)));
            }
        }

        Ok(())
    }

    pub(crate) fn set_nonce_from_msg(&mut self, msg: &Message) {
        // Update nonce
        match Nonce::get_from_as(msg, ATTR_NONCE) {
            Ok(nonce) => {
                self.nonce = nonce;
                log::debug!("refresh allocation: 438, got new nonce.");
            }
            Err(_) => log::warn!("refresh allocation: 438 but no nonce."),
        }
    }

    // Close closes the connection.
    // Any blocked ReadFrom or write_to operations will be unblocked and return errors.
    pub(crate) async fn close(&mut self) -> Result<(), Error> {
        self.refresh_allocation(Duration::from_secs(0), true /* dontWait=true */)
            .await
    }

    async fn refresh_allocation(
        &mut self,
        lifetime: Duration,
        dont_wait: bool,
    ) -> Result<(), Error> {
        let res = {
            let mut obs = self.obs.lock().await;

            let mut msg = Message::new();
            msg.build(&[
                Box::new(TransactionId::new()),
                Box::new(MessageType::new(METHOD_REFRESH, CLASS_REQUEST)),
                Box::new(proto::lifetime::Lifetime(lifetime)),
                Box::new(obs.username()),
                Box::new(obs.realm()),
                Box::new(self.nonce.clone()),
                Box::new(self.integrity.clone()),
                Box::new(FINGERPRINT),
            ])?;

            log::debug!("send refresh request (dont_wait={})", dont_wait);
            let turn_server_addr = obs.turn_server_addr();
            let tr_res = obs
                .perform_transaction(&msg, &turn_server_addr, dont_wait)
                .await?;

            if dont_wait {
                log::debug!("refresh request sent");
                return Ok(());
            }

            log::debug!("refresh request sent, and waiting response");

            tr_res.msg
        };

        if res.typ.class == CLASS_ERROR_RESPONSE {
            let mut code = ErrorCodeAttribute::default();
            let result = code.get_from(&res);
            if result.is_err() {
                return Err(Error::Other(format!("{}", res.typ)));
            } else if code.code == CODE_STALE_NONCE {
                self.set_nonce_from_msg(&res);
                return Err(Error::ErrTryAgain);
            } else {
                return Ok(());
            }
        }

        // Getting lifetime from response
        let mut updated_lifetime = proto::lifetime::Lifetime::default();
        updated_lifetime.get_from(&res)?;

        self.lifetime = updated_lifetime.0;
        log::debug!("updated lifetime: {} seconds", self.lifetime.as_secs());
        Ok(())
    }

    async fn bind(
        rc_obs: Arc<Mutex<T>>,
        bind_addr: SocketAddr,
        bind_number: u16,
        nonce: Nonce,
        integrity: MessageIntegrity,
    ) -> Result<(), Error> {
        let (msg, turn_server_addr) = {
            let obs = rc_obs.lock().await;

            let setters: Vec<Box<dyn Setter>> = vec![
                Box::new(TransactionId::new()),
                Box::new(MessageType::new(METHOD_CHANNEL_BIND, CLASS_REQUEST)),
                Box::new(socket_addr2peer_address(&bind_addr)),
                Box::new(proto::channum::ChannelNumber(bind_number)),
                Box::new(obs.username()),
                Box::new(obs.realm()),
                Box::new(nonce),
                Box::new(integrity),
                Box::new(FINGERPRINT),
            ];

            let mut msg = Message::new();
            msg.build(&setters)?;

            (msg, obs.turn_server_addr())
        };

        log::debug!("UDPConn.bind call PerformTransaction 1");
        let tr_res = {
            let mut obs = rc_obs.lock().await;
            obs.perform_transaction(&msg, &turn_server_addr, false)
                .await?
        };

        let res = tr_res.msg;

        if res.typ != MessageType::new(METHOD_CHANNEL_BIND, CLASS_SUCCESS_RESPONSE) {
            return Err(Error::ErrUnexpectedResponse);
        }

        log::debug!("channel binding successful: {} {}", bind_addr, bind_number);

        // Success.
        Ok(())
    }
}

fn socket_addr2peer_address(addr: &SocketAddr) -> proto::peeraddr::PeerAddress {
    proto::peeraddr::PeerAddress {
        ip: addr.ip(),
        port: addr.port(),
    }
}
