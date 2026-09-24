# Miracast compatibility

This matrix separates protocol coverage from hardware results. A checked code
path is not a claim that a particular television or adapter has been tested.

| Area | Implemented | Automated | Hardware verified |
|---|---:|---:|---:|
| RTSP M1–M7 with CSeq correlation | Yes | Yes | Pending |
| Sequential M4 then M5 | Yes | Yes | Pending |
| CEA progressive modes | Yes | Yes | Pending |
| VESA and handheld modes | Yes | Yes | Pending |
| Interlaced modes | No (rejected) | Yes | N/A |
| H.264 constrained baseline/high negotiation | Yes | Yes | Pending |
| H.264 levels 3.1–4.2 | Yes | Yes | Pending |
| MPEG-TS AVC/LPCM descriptors | Yes | Yes | Pending |
| WFD LPCM, 48 kHz/16-bit/stereo | Yes | Yes | Pending |
| Transport `client_port` override | Yes | Yes | Pending |
| Direct Wi-Fi Direct discovery/pairing | Yes | Partial | Pending |
| Direct-IP diagnostic mode | Yes | Partial | Pending |
| MS-MICE (TCP/7250 control plane) | No | No | No |
| HDCP | No | No | No |

## Adding a device result

Record the vendor, exact model, firmware, Windows version, Wi-Fi adapter and
driver, connection path (Wi-Fi Direct or direct IP), negotiated mode/profile/
level, whether audio remained synchronized for ten minutes, reconnect result,
and any observed failure. Do not mark a family of devices compatible based on
one model.

Run the workspace tests before and after adding a captured capability body.
Keep the fixture free of MAC addresses, IP addresses, PINs and device serials.
Add it as a source capability test in `crates/castr-miracast/src/source/caps.rs`
so future negotiation changes replay the real advertisement.

