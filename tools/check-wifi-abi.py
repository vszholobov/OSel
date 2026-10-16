#!/usr/bin/env python3
"""Regenerate independent wire fixtures using pinned ISC ath10k C declarations.

Download the v6.12 headers listed in fixtures/ath10k-v6.12/sources.json
and the legacy v4.4 HTT header (as htt-v4.4.h) into --reference-dir.
No kernel headers, Rust output, network or installation
are used by this generator. Ordinary cargo tests only read the saved fixtures.
"""
import argparse
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURES = ROOT / 'osel-core/tests/fixtures/ath10k-v6.12'


def declarations(sources):
    joined = '\n'.join(sources)
    joined = re.sub(r'/\*.*?\*/|//[^\n]*', '', joined, flags=re.S)
    found = {}
    for match in re.finditer(r'\b(struct|enum)\s+(\w+)\s*\{', joined):
        level, end = 1, match.end()
        while level:
            level += (joined[end] == '{') - (joined[end] == '}')
            end += 1
        end = joined.index(';', end) + 1
        found[match[1], match[2]] = joined[match.start():end]
    return found


def header(sources, legacy):
    found = declarations(sources)
    emitted = set()
    output = ['''#include <stdint.h>
#include <stddef.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef uint64_t u64;
typedef uint16_t __le16; typedef uint32_t __le32; typedef uint64_t __le64;
#define __packed __attribute__((packed))
#define __aligned(n) __attribute__((aligned(n)))
#define BIT(n) (1U << (n))
#define GENMASK(h,l) ((~0U << (l)) & (~0U >> (31-(h))))
#define DECLARE_FLEX_ARRAY(type,name) type name[0]
#define WMI_TLV_CMD(g) (((g)<<12)|1)
#define WMI_TLV_EV(g) (((g)<<12)|1)
#define WMI_TLV_VDEV_PARAM_UNSUPPORTED 0
#define RX_HTT_HDR_STATUS_LEN 64
#define WMI_TLV_MGMT_RX_NUM_RSSI 4
#if __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error Fixture generation requires a little-endian host
#endif
''']

    def emit(kind, name):
        if (kind, name) in emitted:
            return
        text = found[kind, name]
        emitted.add((kind, name))
        for dependency in re.finditer(r'\b(struct|enum)\s+(\w+)\b', text):
            key = dependency[1], dependency[2]
            if key != (kind, name):
                emit(*key)
        output.append(text)

    for name in ['wmi_tlv_grp_id', 'wmi_tlv_cmd_id', 'wmi_tlv_event_id',
                 'wmi_tlv_tag', 'wmi_tlv_vdev_param', 'wmi_tlv_peer_param', 'wmi_tlv_peer_flags',
                 'wmi_phy_mode', 'wmi_cipher_suites', 'wmi_peer_type', 'ath10k_hw_txrx_mode', 'htt_mgmt_tx_status']:
        emit('enum', name)
    for name in ['wmi_tlv', 'wmi_tlv_init_cmd', 'wmi_tlv_resource_config',
                 'host_memory_chunk_tlv', 'wlan_host_mem_req',
                 'wmi_tlv_svc_rdy_ev', 'hal_reg_capabilities', 'wmi_tlv_rdy_ev',
                 'wmi_vdev_create_cmd', 'wmi_tlv_vdev_start_cmd', 'wmi_vdev_up_cmd',
                 'wmi_vdev_set_param_cmd', 'wmi_peer_set_param_cmd',
                 'wmi_tlv_peer_create_cmd', 'wmi_tlv_peer_assoc_cmd',
                 'wmi_vdev_install_key_cmd', 'wmi_tlv_start_scan_cmd',
                 'wmi_tlv_scan_chan_list_cmd', 'wmi_channel',
                 'wmi_tlv_mgmt_rx_ev', 'wmi_scan_event',
                 'wmi_vdev_start_response_event', 'ath10k_htc_hdr',
                 'ath10k_ath10k_htc_record_hdr', 'ath10k_htc_credit_report',
                 'htt_cmd_hdr', 'htt_ver_resp', 'htt_rx_peer_map',
                 'htt_rx_peer_unmap', 'htt_data_tx_completion',
                 'htt_security_indication', 'htt_mgmt_tx_completion', 'htt_rx_ring_setup_ring32',
                 'htt_data_tx_desc', 'htt_rx_desc_v1']:
        emit('struct', name)
    original = declarations([legacy])['struct', 'htt_mgmt_tx_completion']
    output.append(original.replace('htt_mgmt_tx_completion', 'htt_mgmt_tx_completion_legacy'))
    return '\n'.join(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference-dir', required=True, type=pathlib.Path)
    parser.add_argument('--update', action='store_true')
    args = parser.parse_args()
    manifest = json.loads((FIXTURES / 'sources.json').read_text())
    sources = []
    for name, digest in manifest['files'].items():
        data = (args.reference_dir / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != digest:
            parser.error(f'{name}: does not match the pinned v6.12 source')
        sources.append(data.decode())
    legacy = manifest['legacy_management_completion']
    legacy_data = (args.reference_dir / legacy['file']).read_bytes()
    if hashlib.sha256(legacy_data).hexdigest() != legacy['sha256']:
        parser.error('legacy HTT header: does not match the pinned v4.4 source')
    with tempfile.TemporaryDirectory(prefix='osel-wifi-abi-') as name:
        temporary = pathlib.Path(name)
        (temporary / 'ath10k-reference.h').write_text(header(sources, legacy_data.decode()))
        subprocess.run(['cc', '-std=gnu11', '-Wall', '-Wextra', '-Werror',
                        '-I', str(temporary), str(ROOT / 'tools/wifi-abi-cases.c'),
                        '-o', str(temporary / 'reference')], check=True)
        subprocess.run([str(temporary / 'reference'), str(temporary)], check=True)
        generated = sorted(temporary.glob('*.bin'))
        for fixture in generated:
            target = FIXTURES / fixture.name
            if args.update:
                shutil.copyfile(fixture, target)
            elif not target.exists() or fixture.read_bytes() != target.read_bytes():
                parser.error(f'{target.name}: fixture differs from the C reference')
        expected = {path.name for path in FIXTURES.glob('*.bin')}
        if expected != {path.name for path in generated}:
            parser.error('unexpected or missing fixture files')
        print(f'{len(generated)} fixtures match pinned ath10k C declarations')


if __name__ == '__main__':
    main()
