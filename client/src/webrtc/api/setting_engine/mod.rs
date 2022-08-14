
use crate::webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;
use crate::webrtc::ice::agent::agent_config::InterfaceFilterFn;
use crate::webrtc::ice::mdns::MulticastDnsMode;
use crate::webrtc::ice::network_type::NetworkType;

use std::sync::Arc;

#[derive(Default, Clone)]
pub(crate) struct Candidates {
    pub(crate) ice_lite: bool,
    pub(crate) ice_network_types: Vec<NetworkType>,
    pub(crate) interface_filter: Arc<Option<InterfaceFilterFn>>,
    pub(crate) nat_1to1_ips: Vec<String>,
    pub(crate) nat_1to1_ip_candidate_type: RTCIceCandidateType,
    pub(crate) multicast_dns_mode: MulticastDnsMode,
    pub(crate) multicast_dns_host_name: String,
    pub(crate) username_fragment: String,
    pub(crate) password: String,
}

/// SettingEngine allows influencing behavior in ways that are not
/// supported by the WebRTC API. This allows us to support additional
/// use-cases without deviating from the WebRTC API elsewhere.
#[derive(Default, Clone)]
pub(crate) struct SettingEngine {
    pub(crate) candidates: Candidates,
}

impl SettingEngine {
    pub(crate) fn new() -> Self {
        let setting_engine = Self::default();
        setting_engine
    }
}
