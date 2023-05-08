use std::sync::Arc;

use bytes::Bytes;
use reqwest::{Client as HttpClient, Response};
use thiserror::Error;
use tinyjson::JsonValue;
use tokio::sync::mpsc;

use crate::webrtc::{
    data_channel::internal::data_channel::DataChannel,
    peer_connection::{sdp::session_description::RTCSessionDescription, RTCPeerConnection},
};

use super::addr_cell::AddrCell;

const MESSAGE_SIZE: usize = 1500;

pub struct Socket {
    addr_cell: AddrCell,
    to_server_receiver: mpsc::UnboundedReceiver<Box<[u8]>>,
    to_client_sender: mpsc::UnboundedSender<Box<[u8]>>,
}

pub struct SocketIo {
    pub addr_cell: AddrCell,
    pub to_server_sender: mpsc::UnboundedSender<Box<[u8]>>,
    pub to_client_receiver: mpsc::UnboundedReceiver<Box<[u8]>>,
}

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SocketConnectionError {
    #[error("webrtc error")]
    WebrtcError(crate::webrtc::error::Error),
    #[error("session request error")]
    SessionRequestError(reqwest::Error),
}

impl Socket {
    pub fn new() -> (Self, SocketIo) {
        let addr_cell = AddrCell::default();
        let (to_server_sender, to_server_receiver) = mpsc::unbounded_channel();
        let (to_client_sender, to_client_receiver) = mpsc::unbounded_channel();

        (
            Self {
                addr_cell: addr_cell.clone(),
                to_server_receiver,
                to_client_sender,
            },
            SocketIo {
                addr_cell,
                to_server_sender,
                to_client_receiver,
            },
        )
    }

    pub async fn connect(self, server_url: &str) -> Result<(), SocketConnectionError> {
        let Self {
            addr_cell,
            to_server_receiver,
            to_client_sender,
        } = self;

        // create a new RTCPeerConnection
        let peer_connection = RTCPeerConnection::new().await;

        let label = "data";
        let protocol = "";

        // create a datachannel with label 'data'
        let data_channel = peer_connection
            .create_data_channel(label, protocol)
            .await
            .expect("cannot create data channel");

        // datachannel on_error callback
        data_channel
            .on_error(Box::new(move |error| {
                log::warn!("data channel error: {:?}", error);
                Box::pin(async {})
            }))
            .await;

        // datachannel on_open callback
        let data_channel_ref = Arc::clone(&data_channel);
        data_channel
            .on_open(Box::new(move || {
                let data_channel_ref_2 = Arc::clone(&data_channel_ref);
                Box::pin(async move {
                    // The `detach` call can fail only if the channel isn't opened yet,
                    // but we are in the `on_open` handler, hence the panic.
                    let detached_data_channel = data_channel_ref_2
                        .detach()
                        .await
                        .expect("data channel detach got error");

                    // Handle reading from the data channel
                    let detached_data_channel_1 = Arc::clone(&detached_data_channel);
                    let detached_data_channel_2 = Arc::clone(&detached_data_channel);
                    tokio::spawn(async move {
                        let _loop_result =
                            read_loop(detached_data_channel_1, to_client_sender).await;
                        // do nothing with result, just close thread
                    });

                    // Handle writing to the data channel
                    tokio::spawn(async move {
                        let _loop_result =
                            write_loop(detached_data_channel_2, to_server_receiver).await;
                        // do nothing with result, just close thread
                    });
                })
            }))
            .await;

        // create an offer to send to the server
        let offer = peer_connection
            .create_offer()
            .await
            .map_err(SocketConnectionError::WebrtcError)?;

        // sets the LocalDescription, and starts our UDP listeners
        peer_connection
            .set_local_description(offer)
            .await
            .map_err(SocketConnectionError::WebrtcError)?;

        // send a request to server to initiate connection (signaling, essentially)
        let http_client = HttpClient::new();

        let sdp = peer_connection.local_description().await.unwrap().sdp;

        let sdp_len = sdp.len();

        // wait to receive a response from server
        let response: Response = {
            let request = http_client
                .post(server_url)
                .header("Content-Length", sdp_len)
                .body(sdp.clone());

            request
                .send()
                .await
                .map_err(SocketConnectionError::SessionRequestError)?
        };
        let response_string = response
            .text()
            .await
            .map_err(SocketConnectionError::SessionRequestError)?;

        // parse session from server response
        let session_response: JsSessionResponse = get_session_response(response_string.as_str());

        // apply the server's response as the remote description
        let session_description =
            RTCSessionDescription::answer(session_response.answer.sdp).unwrap();

        peer_connection
            .set_remote_description(session_description)
            .await
            .map_err(SocketConnectionError::WebrtcError)?;

        addr_cell
            .receive_candidate(session_response.candidate.candidate.as_str())
            .await;

        // add ice candidate to connection
        peer_connection
            .add_ice_candidate(session_response.candidate.candidate)
            .await
            .map_err(SocketConnectionError::WebrtcError)?;

        Ok(())
    }
}

// read_loop shows how to read from the datachannel directly
async fn read_loop(
    data_channel: Arc<DataChannel>,
    to_client_sender: mpsc::UnboundedSender<Box<[u8]>>,
) -> Result<(), mpsc::error::SendError<Box<[u8]>>> {
    let mut buffer = vec![0u8; MESSAGE_SIZE];
    loop {
        let message_length = match data_channel.read(&mut buffer).await {
            Ok(length) => length,
            Err(err) => {
                log::debug!("Datachannel closed; Exit the read_loop: {}", err);
                return Ok(());
            }
        };

        to_client_sender.send(buffer[..message_length].into())?;
    }
}

// write_loop shows how to write to the datachannel directly
async fn write_loop(
    data_channel: Arc<DataChannel>,
    mut to_server_receiver: mpsc::UnboundedReceiver<Box<[u8]>>,
) -> crate::webrtc::data_channel::Result<()> {
    loop {
        if let Some(write_message) = to_server_receiver.recv().await {
            data_channel.write(&Bytes::from(write_message)).await?;
        } else {
            return Ok(());
        }
    }
}

#[derive(Clone)]
pub(crate) struct SessionAnswer {
    pub(crate) sdp: String,
}

pub(crate) struct SessionCandidate {
    pub(crate) candidate: String,
}

pub(crate) struct JsSessionResponse {
    pub(crate) answer: SessionAnswer,
    pub(crate) candidate: SessionCandidate,
}

fn get_session_response(input: &str) -> JsSessionResponse {
    let json_obj: JsonValue = input.parse().unwrap();

    let sdp_opt: Option<&String> = json_obj["answer"]["sdp"].get();
    let sdp: String = sdp_opt.unwrap().clone();

    let candidate_opt: Option<&String> = json_obj["candidate"]["candidate"].get();
    let candidate: String = candidate_opt.unwrap().clone();

    JsSessionResponse {
        answer: SessionAnswer { sdp },
        candidate: SessionCandidate { candidate },
    }
}
