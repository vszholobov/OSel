/* Independent ABI fixtures, built against original pinned ath10k declarations.
 * The generator extracts ISC declarations into a temporary header. See
 * osel-core/tests/fixtures/ath10k-v6.12/sources.json and firmware/ath10k-driver-ISC.txt.
 * These are synthetic protocol examples, not captured radio traffic or keys.
 */
#include "ath10k-reference.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct tlv_header {__le16 len; __le16 tag;} __packed;
_Static_assert(sizeof(struct tlv_header) == sizeof(struct wmi_tlv), "TLV header size");

static unsigned char bytes[8192];
static size_t length;
static const char *destination;
static const u8 ap[6] = {2, 3, 4, 5, 6, 7};
static const u8 sta[6] = {2, 3, 4, 5, 6, 8};
static const u8 rates[4] = {2, 4, 11, 22};
static const struct wmi_tlv_abi_version abi = {
    .abi_ver0 = 0x01000000, .abi_ver1 = 53,
    .abi_ver_ns0 = 0x5f414351, .abi_ver_ns1 = 0x4c4d,
};

static void append(const void *value, size_t size) {
    if (length + size > sizeof(bytes)) abort();
    if (size) memcpy(bytes + length, value, size);
    length += size;
}
static void word(u32 value) { append(&value, sizeof(value)); }
static void begin(u32 id) { length = 0; word(id); }
static void tlv(u16 tag, const void *value, size_t size) {
    struct wmi_tlv header = {.tag = tag, .len = (u16)size};
    append(&header, sizeof(header));
    append(value, size);
}
static void save(const char *name) {
    char path[4096];
    if (snprintf(path, sizeof(path), "%s/%s.bin", destination, name) >= (int)sizeof(path)) abort();
    FILE *file = fopen(path, "wb");
    if (!file || fwrite(bytes, 1, length, file) != length || fclose(file)) abort();
}

static void startup(void) {
    struct wmi_tlv_svc_rdy_ev service = {
        .fw_build_vers = 0x441a8135, .abi = abi, .num_rf_chains = 2, .num_mem_reqs = 1,
    };
    struct hal_reg_capabilities reg = {
        .eeprom_rd = 0x6c, .low_2ghz_chan = 2412, .high_2ghz_chan = 2472,
        .low_5ghz_chan = 5180, .high_5ghz_chan = 5825,
    };
    u32 services[32] = {0};
    services[65 / 4] = 1U << (65 % 4);
    struct wlan_host_mem_req request = {.req_id = 1, .unit_size = 32, .num_unit_info = 2};
    begin(WMI_TLV_SERVICE_READY_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_SERVICE_READY_EVENT, &service, sizeof(service));
    tlv(WMI_TLV_TAG_STRUCT_HAL_REG_CAPABILITIES, &reg, sizeof(reg));
    tlv(WMI_TLV_TAG_ARRAY_UINT32, services, sizeof(services));
    struct {struct tlv_header header; struct wlan_host_mem_req request;} __packed nested = {
        .header = {.tag = WMI_TLV_TAG_STRUCT_WLAN_HOST_MEM_REQ, .len = sizeof(request)},
        .request = request,
    };
    tlv(WMI_TLV_TAG_ARRAY_STRUCT, &nested, sizeof(nested));
    save("service-ready");
    struct wmi_tlv_rdy_ev ready = {.abi = abi};
    memcpy(ready.mac_addr.addr, sta, 6);
    begin(WMI_TLV_READY_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_READY_EVENT, &ready, sizeof(ready));
    save("ready");

    struct wmi_tlv_init_cmd init = {.abi = abi, .num_host_mem_chunks = 1};
    struct wmi_tlv_resource_config resource = {
        .num_vdevs = 4, .num_peers = 33, .num_offload_peers = 4, .num_offload_reorder_bufs = 4,
        .num_peer_keys = 2, .num_tids = 66, .ast_skid_limit = 16,
        .tx_chain_mask = 3, .rx_chain_mask = 3, .rx_timeout_pri = {100, 100, 100, 40},
        .rx_decap_mode = 2, .scan_max_pending_reqs = 4, .bmiss_offload_max_vdev = 4,
        .roam_offload_max_vdev = 4, .roam_offload_max_ap_profiles = 8,
        .tx_dbg_log_size = 0x400, .num_wds_entries = 32, .gtk_offload_max_vdev = 2,
        .num_msdu_desc = 64, .max_frag_entries = 2, .num_multicast_filter_entries = 5,
        .num_wow_filters = 22, .num_keep_alive_pattern = 6,
        .max_tdls_concurrent_sleep_sta = 1, .max_tdls_concurrent_buffer_sta = 1,
    };
    struct {struct tlv_header header; struct host_memory_chunk_tlv memory;} __packed chunk = {
        .header = {.tag = WMI_TLV_TAG_STRUCT_WLAN_HOST_MEMORY_CHUNK, .len = sizeof(chunk.memory)},
        .memory = {.req_id = 1, .ptr = 0x1000, .size = 34 * 32},
    };
    begin(WMI_TLV_INIT_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_INIT_CMD, &init, sizeof(init));
    tlv(WMI_TLV_TAG_STRUCT_RESOURCE_CONFIG, &resource, sizeof(resource));
    tlv(WMI_TLV_TAG_ARRAY_STRUCT, &chunk, sizeof(chunk));
    save("init");
}

static void scanning(void) {
    u32 frequencies[] = {2412, 2437};
    struct wmi_tlv_scan_chan_list_cmd list = {.num_scan_chans = 2};
    struct {struct tlv_header header; struct wmi_channel channel;} __packed channels[2] = {0};
    for (size_t i = 0; i < 2; i++) {
        channels[i].header.tag = WMI_TLV_TAG_STRUCT_CHANNEL;
        channels[i].header.len = sizeof(struct wmi_channel);
        channels[i].channel.mhz = frequencies[i];
        channels[i].channel.band_center_freq1 = frequencies[i];
        channels[i].channel.flags = MODE_11G | (1U << 7);
    }
    begin(WMI_TLV_SCAN_CHAN_LIST_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_SCAN_CHAN_LIST_CMD, &list, sizeof(list));
    tlv(WMI_TLV_TAG_ARRAY_STRUCT, channels, sizeof(channels));
    save("scan-channels");
    struct wmi_tlv_start_scan_cmd scan = {
        .common = {.scan_id = 0xa001, .scan_req_id = 0xa001, .notify_scan_events = 0x14b,
                   .dwell_time_active = 50, .dwell_time_passive = 200,
                   .min_rest_time = 50, .max_rest_time = 500,
                   .max_scan_time = 10000, .scan_ctrl_flags = 0x21},
        .num_channels = 2, .num_bssids = 1, .num_probes = 3,
    };
    struct wmi_mac_addr broadcast = {.addr = {255, 255, 255, 255, 255, 255}};
    begin(WMI_TLV_START_SCAN_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_START_SCAN_CMD, &scan, sizeof(scan));
    tlv(WMI_TLV_TAG_ARRAY_UINT32, frequencies, sizeof(frequencies));
    tlv(WMI_TLV_TAG_ARRAY_FIXED_STRUCT, NULL, 0);
    tlv(WMI_TLV_TAG_ARRAY_FIXED_STRUCT, &broadcast, sizeof(broadcast));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, NULL, 0);
    save("start-scan");
    struct { struct tlv_header header; struct wmi_channel channel; } __packed receive5 = {
        .header = {.tag = WMI_TLV_TAG_STRUCT_CHANNEL, .len = sizeof(struct wmi_channel)},
        .channel = {.mhz = 5180, .band_center_freq1 = 5180, .flags = MODE_11A | (1U << 7)},
    };
    list.num_scan_chans = 1;
    begin(WMI_TLV_SCAN_CHAN_LIST_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_SCAN_CHAN_LIST_CMD, &list, sizeof(list));
    tlv(WMI_TLV_TAG_ARRAY_STRUCT, &receive5, sizeof(receive5));
    save("scan-channel-5ghz");
}

static void station_commands(void) {
    struct wmi_vdev_create_cmd create = {.vdev_type = 2};
    memcpy(create.vdev_macaddr.addr, sta, 6);
    begin(WMI_TLV_VDEV_CREATE_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_VDEV_CREATE_CMD, &create, sizeof(create));
    save("vdev-create");
    struct wmi_vdev_set_param_cmd param = {.param_id = WMI_TLV_VDEV_PARAM_TX_ENCAP_TYPE, .param_value = ATH10K_HW_TXRX_NATIVE_WIFI};
    begin(WMI_TLV_VDEV_SET_PARAM_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_VDEV_SET_PARAM_CMD, &param, sizeof(param));
    save("tx-encapsulation");
    struct wmi_tlv_vdev_start_cmd start = {.bcn_intval = 100, .dtim_period = 1, .ssid = {.ssid_len = 4, .ssid = "OSel"}};
    struct wmi_channel channel = {.mhz = 2412, .band_center_freq1 = 2412, .flags = MODE_11G};
    struct wmi_tlv_scan_chan_list_cmd list = {.num_scan_chans = 1};
    struct {struct tlv_header header; struct wmi_channel channel;} __packed home = {
        .header = {.tag = WMI_TLV_TAG_STRUCT_CHANNEL, .len = sizeof(channel)},
        .channel = channel,
    };
    begin(WMI_TLV_SCAN_CHAN_LIST_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_SCAN_CHAN_LIST_CMD, &list, sizeof(list));
    tlv(WMI_TLV_TAG_ARRAY_STRUCT, &home, sizeof(home));
    save("station-channel");
    struct wmi_tlv_pdev_set_rd_cmd domain = {
        .regd = reference_world.domain, .regd_2ghz = reference_world.domain,
        .regd_5ghz = reference_world.domain,
        .conform_limit_2ghz = reference_world.ctl2g, .conform_limit_5ghz = reference_world.ctl5g,
    };
    begin(WMI_TLV_PDEV_SET_REGDOMAIN_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_PDEV_SET_REGDOMAIN_CMD, &domain, sizeof(domain));
    save("station-regdomain");
    begin(WMI_TLV_SERVICE_MGMT_TX_HTT); word(WMI_TLV_SERVICE_MGMT_TX_WMI);
    save("management-services");
    begin(WMI_TLV_VDEV_START_REQUEST_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_VDEV_START_REQUEST_CMD, &start, sizeof(start));
    tlv(WMI_TLV_TAG_STRUCT_CHANNEL, &channel, sizeof(channel));
    tlv(WMI_TLV_TAG_ARRAY_STRUCT, NULL, 0);
    save("vdev-start");
    struct wmi_tlv_peer_create_cmd peer = {.peer_type = WMI_PEER_TYPE_DEFAULT};
    memcpy(peer.peer_addr.addr, ap, 6);
    begin(WMI_TLV_PEER_CREATE_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_PEER_CREATE_CMD, &peer, sizeof(peer));
    save("peer-create");
    struct wmi_tlv_peer_assoc_cmd assoc = {
        .new_assoc = 1, .assoc_id = 0x123,
        .flags = WMI_TLV_PEER_AUTH | WMI_TLV_PEER_NEED_PTK_4_WAY,
        .caps = 0x11, .listen_intval = 1, .nss = 1, .phy_mode = MODE_11G, .num_legacy_rates = 4,
    };
    memcpy(assoc.mac_addr.addr, ap, 6);
    begin(WMI_TLV_PEER_ASSOC_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_PEER_ASSOC_COMPLETE_CMD, &assoc, sizeof(assoc));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, rates, sizeof(rates));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, NULL, 0);
    u32 vht_rates[4] = {0};
    tlv(WMI_TLV_TAG_STRUCT_VHT_RATE_SET, vht_rates, sizeof(vht_rates));
    save("peer-associate");
    struct wmi_vdev_up_cmd up = {.vdev_assoc_id = 0x123};
    memcpy(up.vdev_bssid.addr, ap, 6);
    begin(WMI_TLV_VDEV_UP_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_VDEV_UP_CMD, &up, sizeof(up));
    save("vdev-up");
    struct wmi_vdev_install_key_cmd key = {.key_idx = 2, .key_flags = 1, .key_cipher = WMI_CIPHER_AES_CCM, .key_len = 16};
    memcpy(key.peer_macaddr.addr, ap, 6);
    const u8 rsc[8] = {1, 2, 3, 4, 5, 6, 0, 0};
    memcpy(&key.key_rsc_counter, rsc, sizeof(rsc));
    u8 material[16];
    for (size_t i = 0; i < sizeof(material); i++) material[i] = (u8)(0xa0 + i);
    begin(WMI_TLV_VDEV_INSTALL_KEY_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_VDEV_INSTALL_KEY_CMD, &key, sizeof(key));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, material, sizeof(material));
    save("group-key");
    struct wmi_peer_set_param_cmd peer_param = {.param_id = WMI_TLV_PEER_AUTHORIZE, .param_value = 1};
    memcpy(peer_param.peer_macaddr.addr, ap, 6);
    begin(WMI_TLV_PEER_SET_PARAM_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_PEER_SET_PARAM_CMD, &peer_param, sizeof(peer_param));
    save("peer-authorize");
    peer_param.param_id = WMI_TLV_PEER_DUMMY_VAR;
    begin(WMI_TLV_PEER_SET_PARAM_CMDID);
    tlv(WMI_TLV_TAG_STRUCT_PEER_SET_PARAM_CMD, &peer_param, sizeof(peer_param));
    save("peer-wake");
}

static void station_events(void) {
    struct wmi_vdev_start_response_event started = {0};
    begin(WMI_TLV_VDEV_START_RESP_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_VDEV_START_RESPONSE_EVENT, &started, sizeof(started));
    save("vdev-started");
    struct wmi_scan_event scan = {
        .event_type = 2, .channel_freq = 2412, .scan_req_id = 0xa001, .scan_id = 0xa001,
    };
    begin(WMI_TLV_SCAN_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_SCAN_EVENT, &scan, sizeof(scan));
    save("scan-completed");
    u8 frame[72] = {0};
    frame[0] = 0x80;
    memset(frame + 4, 255, 6); memcpy(frame + 10, ap, 6); memcpy(frame + 16, ap, 6);
    frame[32] = 100; frame[34] = 0x11;
    const u8 ies[] = {0, 4, 'O', 'S', 'e', 'l', 1, 4, 0x82, 0x84, 0x8b, 0x96,
        48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2, 0, 0};
    memcpy(frame + 36, ies, sizeof(ies));
    struct wmi_tlv_mgmt_rx_ev header = {.channel = 1, .snr = 45, .phy_mode = MODE_11G, .buf_len = 70};
    begin(WMI_TLV_MGMT_RX_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR, &header, sizeof(header));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, frame, sizeof(frame));
    save("beacon");
    memset(frame, 0, sizeof(frame));
    frame[0] = 0xb0; memcpy(frame + 4, sta, 6); memcpy(frame + 10, ap, 6); memcpy(frame + 16, ap, 6);
    frame[26] = 2; header.buf_len = 30;
    begin(WMI_TLV_MGMT_RX_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR, &header, sizeof(header));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, frame, 32);
    save("authenticated");
    frame[0] = 0x10; frame[24] = 0x11; frame[26] = 0; frame[28] = 0x23; frame[29] = 0xc1;
    begin(WMI_TLV_MGMT_RX_EVENTID);
    tlv(WMI_TLV_TAG_STRUCT_MGMT_RX_HDR, &header, sizeof(header));
    tlv(WMI_TLV_TAG_ARRAY_BYTE, frame, 32);
    save("associated");
}

static void transport(void) {
    struct ath10k_htc_hdr header = {.eid = 1, .flags = 2, .len = 12, .trailer_len = 8};
    struct htt_ver_resp version = {.minor = 1, .major = 3};
    struct ath10k_ath10k_htc_record_hdr record = {.id = 1, .len = 4, .pad0 = 0xa5, .pad1 = 0x5a};
    struct ath10k_htc_credit_report credit = {.eid = 2, .credits = 1, .pad0 = 0xaa, .pad1 = 0xbb};
    length = 0;
    append(&header, sizeof(header));
    u8 type = 0;
    append(&type, 1); append(&version, sizeof(version));
    append(&record, sizeof(record)); append(&credit, sizeof(credit));
    save("htt-version-with-credits");
    header.eid = 0; header.len = 8;
    length = 0; append(&header, sizeof(header));
    append(&record, sizeof(record)); append(&credit, sizeof(credit));
    save("htc-credit-only");
    struct htt_rx_peer_map peer = {.vdev_id = 0, .peer_id = 7};
    memcpy(peer.addr, ap, 6);
    length = 0; type = 3; append(&type, 1); append(&peer, sizeof(peer)); save("peer-map");
    struct htt_security_indication security = {.flags = 0x86, .peer_id = 7};
    length = 0; type = 0xb; append(&type, 1); append(&security, sizeof(security)); save("security");
    security.flags = 6;
    length = 0; append(&type, 1); append(&security, sizeof(security)); save("group-security");
    struct htt_rx_peer_unmap unmap = {.peer_id = 7};
    length = 0; type = 4; append(&type, 1); append(&unmap, sizeof(unmap)); save("peer-unmap");
    struct htt_data_tx_completion complete = {.num_msdus = 1};
    length = 0; type = 7; append(&type, 1); append(&complete, sizeof(complete));
    u16 id = 0; append(&id, 2); id = 0xffff; append(&id, 2); save("tx-completed");
    struct htt_mgmt_tx_completion mgmt = {.desc_id = 0, .status = HTT_MGMT_TX_STATUS_OK};
    length = 0; type = 0xe; append(&type, 1); append(&mgmt, sizeof(mgmt)); save("management-completed");
    struct htt_mgmt_tx_completion_legacy legacy = {.desc_id = 0, .status = HTT_MGMT_TX_STATUS_OK};
    length = 0; append(&type, 1); append(&legacy, sizeof(legacy)); save("management-completed-legacy");
    struct htt_rx_ring_setup_ring32 ring = {
        .fw_idx_shadow_reg_paddr = 0x1000, .rx_ring_base_paddr = 0x2000,
        .rx_ring_len = 128, .rx_ring_bufsize = 2048, .flags = 0xffff, .fw_idx_init_val = 63,
        .offsets = {
            .mac80211_hdr_offset = offsetof(struct htt_rx_desc_v1, rx_hdr_status) / 4,
            .msdu_payload_offset = offsetof(struct htt_rx_desc_v1, msdu_payload) / 4,
            .ppdu_start_offset = offsetof(struct htt_rx_desc_v1, ppdu_start) / 4,
            .ppdu_end_offset = offsetof(struct htt_rx_desc_v1, ppdu_end) / 4,
            .mpdu_start_offset = offsetof(struct htt_rx_desc_v1, mpdu_start) / 4,
            .mpdu_end_offset = offsetof(struct htt_rx_desc_v1, mpdu_end) / 4,
            .msdu_start_offset = offsetof(struct htt_rx_desc_v1, msdu_start) / 4,
            .msdu_end_offset = offsetof(struct htt_rx_desc_v1, msdu_end) / 4,
            .rx_attention_offset = offsetof(struct htt_rx_desc_v1, attention) / 4,
            .frag_info_offset = offsetof(struct htt_rx_desc_v1, frag_info) / 4,
        },
    };
    begin(0x00000102); append(&ring, sizeof(ring)); save("rx-ring");
    struct htt_rx_desc_v1 rx = {0};
    rx.attention.flags = 0x80000040;
    rx.mpdu_start.info0 = 0x60002000; rx.mpdu_start.pn31_0 = 7;
    rx.msdu_start.common.info1 = 0x200; rx.msdu_end.common.info0 = 0xc000;
    rx.rx_hdr_status[0] = 8; rx.rx_hdr_status[1] = 0x42;
    memcpy(rx.rx_hdr_status + 4, sta, 6); memcpy(rx.rx_hdr_status + 10, ap, 6);
    memcpy(rx.rx_hdr_status + 16, ap, 6);
    length = 0; append(&rx, offsetof(struct htt_rx_desc_v1, msdu_payload));
    append(sta, 6); append(ap, 6); u16 ether_type = 8; append(&ether_type, 2);
    save("encrypted-ethernet");
    struct htt_cmd_hdr command = {.msg_type = 1};
    struct htt_data_tx_desc tx = {
        .flags0 = 0x65, .flags1 = (17U << 6) | (1U << 11), .len = 25,
        .id = 3, .frags_paddr = 0x1000, .peerid = 0xffff,
    };
    u8 frame[64]; memset(frame, 0xab, sizeof(frame));
    length = 0; append(&command, sizeof(command)); append(&tx, sizeof(tx)); append(frame, 25);
    while (length % 4) {u8 zero = 0; append(&zero, 1);}
    save("tx-management");
    tx.flags0 = 0x40; tx.flags1 = (16U << 6) | (1U << 11);
    tx.len = 60; tx.id = 4; tx.frags_paddr = 0x2000;
    memset(frame, 0xcd, sizeof(frame));
    length = 0; append(&command, sizeof(command)); append(&tx, sizeof(tx)); append(frame, 60);
    save("tx-ethernet");
}

int main(int argc, char **argv) {
    if (argc != 2) return 1;
    destination = argv[1];
    startup(); scanning(); station_commands(); station_events(); transport();
    return 0;
}
