use std::fmt;
use std::sync::Arc;

use crate::webrtc::ice::candidate::candidate_base::CandidateBaseConfig;
use crate::webrtc::ice::candidate::candidate_host::CandidateHostConfig;
use crate::webrtc::ice::candidate::candidate_peer_reflexive::CandidatePeerReflexiveConfig;
use crate::webrtc::ice::candidate::candidate_relay::CandidateRelayConfig;
use crate::webrtc::ice::candidate::candidate_server_reflexive::CandidateServerReflexiveConfig;
use crate::webrtc::ice::candidate::Candidate;
use serde::{Deserialize, Serialize};

use crate::webrtc::error::{Error, Result};
use crate::webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;
use crate::webrtc::ice_transport::ice_protocol::RTCIceProtocol;

/// ICECandidate represents a ice candidate
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RTCIceCandidate {
    pub(crate) stats_id: String,
    pub(crate) foundation: String,
    pub(crate) priority: u32,
    pub(crate) address: String,
    pub(crate) protocol: RTCIceProtocol,
    pub(crate) port: u16,
    pub(crate) typ: RTCIceCandidateType,
    pub(crate) component: u16,
    pub(crate) related_address: String,
    pub(crate) related_port: u16,
    pub(crate) tcp_type: String,
}

/// Conversion for ice_candidates
pub(crate) fn rtc_ice_candidates_from_ice_candidates(
    ice_candidates: &[Arc<dyn Candidate + Send + Sync>],
) -> Vec<RTCIceCandidate> {
    ice_candidates.iter().map(|c| c.into()).collect()
}

impl From<&Arc<dyn Candidate + Send + Sync>> for RTCIceCandidate {
    fn from(c: &Arc<dyn Candidate + Send + Sync>) -> Self {
        let typ: RTCIceCandidateType = c.candidate_type().into();
        let protocol = RTCIceProtocol::from(c.network_type().network_short().as_str());
        let (related_address, related_port) = if let Some(ra) = c.related_address() {
            (ra.address, ra.port)
        } else {
            (String::new(), 0)
        };

        RTCIceCandidate {
            stats_id: c.id(),
            foundation: c.foundation(),
            priority: c.priority(),
            address: c.address(),
            protocol,
            port: c.port(),
            component: c.component(),
            typ,
            tcp_type: c.tcp_type().to_string(),
            related_address,
            related_port,
        }
    }
}

impl RTCIceCandidate {
    pub(crate) async fn to_ice(&self) -> Result<impl Candidate> {
        let candidate_id = self.stats_id.clone();
        let c = match self.typ {
            RTCIceCandidateType::Host => {
                let config = CandidateHostConfig {
                    base_config: CandidateBaseConfig {
                        candidate_id,
                        network: self.protocol.to_string(),
                        address: self.address.clone(),
                        port: self.port,
                        component: self.component,
                        //tcp_type: ice.NewTCPType(c.TCPType),
                        foundation: self.foundation.clone(),
                        priority: self.priority,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                config.new_candidate_host().await?
            }
            RTCIceCandidateType::Srflx => {
                let config = CandidateServerReflexiveConfig {
                    base_config: CandidateBaseConfig {
                        candidate_id,
                        network: self.protocol.to_string(),
                        address: self.address.clone(),
                        port: self.port,
                        component: self.component,
                        foundation: self.foundation.clone(),
                        priority: self.priority,
                        ..Default::default()
                    },
                    rel_addr: self.related_address.clone(),
                    rel_port: self.related_port,
                };
                config.new_candidate_server_reflexive().await?
            }
            RTCIceCandidateType::Prflx => {
                let config = CandidatePeerReflexiveConfig {
                    base_config: CandidateBaseConfig {
                        candidate_id,
                        network: self.protocol.to_string(),
                        address: self.address.clone(),
                        port: self.port,
                        component: self.component,
                        foundation: self.foundation.clone(),
                        priority: self.priority,
                        ..Default::default()
                    },
                    rel_addr: self.related_address.clone(),
                    rel_port: self.related_port,
                };
                config.new_candidate_peer_reflexive().await?
            }
            RTCIceCandidateType::Relay => {
                let config = CandidateRelayConfig {
                    base_config: CandidateBaseConfig {
                        candidate_id,
                        network: self.protocol.to_string(),
                        address: self.address.clone(),
                        port: self.port,
                        component: self.component,
                        foundation: self.foundation.clone(),
                        priority: self.priority,
                        ..Default::default()
                    },
                    rel_addr: self.related_address.clone(),
                    rel_port: self.related_port,
                    relay_client: None, //TODO?
                };
                config.new_candidate_relay().await?
            }
            _ => return Err(Error::ErrICECandidateTypeUnknown),
        };

        Ok(c)
    }
}

impl fmt::Display for RTCIceCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {}:{}{}",
            self.protocol, self.typ, self.address, self.port, self.related_address,
        )
    }
}

/// ICECandidateInit is used to serialize ice candidates
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RTCIceCandidateInit {
    pub(crate) candidate: String,
    pub(crate) sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    pub(crate) sdp_mline_index: Option<u16>,
    pub(crate) username_fragment: Option<String>,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_ice_candidate_serialization() {
        let tests = vec![
            (
                RTCIceCandidateInit {
                    candidate: "candidate:abc123".to_string(),
                    sdp_mid: Some("0".to_string()),
                    sdp_mline_index: Some(0),
                    username_fragment: Some("def".to_string()),
                },
                r#"{"candidate":"candidate:abc123","sdpMid":"0","sdpMLineIndex":0,"usernameFragment":"def"}"#,
            ),
            (
                RTCIceCandidateInit {
                    candidate: "candidate:abc123".to_string(),
                    sdp_mid: None,
                    sdp_mline_index: None,
                    username_fragment: None,
                },
                r#"{"candidate":"candidate:abc123","sdpMid":null,"sdpMLineIndex":null,"usernameFragment":null}"#,
            ),
        ];

        for (candidate_init, expected_string) in tests {
            let result = serde_json::to_string(&candidate_init);
            assert!(result.is_ok(), "testCase: marshal err: {:?}", result);
            let candidate_data = result.unwrap();
            assert_eq!(candidate_data, expected_string, "string is not expected");

            let result = serde_json::from_str::<RTCIceCandidateInit>(&candidate_data);
            assert!(result.is_ok(), "testCase: unmarshal err: {:?}", result);
            if let Ok(actual_candidate_init) = result {
                assert_eq!(candidate_init, actual_candidate_init);
            }
        }
    }
}
