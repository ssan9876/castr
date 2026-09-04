//! A DHCP server with exactly one address to give away.
//!
//! Miracast leaves addressing to DHCP, and the Pi has no DHCP server
//! installed, so the sink answers on the group interface itself. The range is
//! deliberately far from the Pi's own LAN so the source cannot confuse the two
//! default routes.

use std::net::Ipv4Addr;

const MAGIC: [u8; 4] = [99, 130, 83, 99];
const OPT_SUBNET: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_CLIENT_ID: u8 = 61;
const OPT_END: u8 = 255;
/// Offset of the fixed header's `yiaddr`, `siaddr` and `chaddr` fields, and of
/// the option area that follows the magic cookie.
/// BOOTP's minimum message length, which some clients still require.
const MIN_MESSAGE: usize = 300;
const OFF_YIADDR: usize = 16;
const OFF_SIADDR: usize = 20;
const OFF_CHADDR: usize = 28;
const OFF_MAGIC: usize = 236;
const OFF_OPTIONS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Discover,
    Request,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub kind: Kind,
    pub xid: u32,
    pub mac: [u8; 6],
    pub requested_ip: Option<Ipv4Addr>,
    /// The client identifier the client sent, if it sent one, kept verbatim.
    /// A reply has to hand it straight back (RFC 6842): Windows sends one in
    /// every message and silently discards any reply that does not echo it,
    /// which reads exactly like a server that is not there.
    pub client_id: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub server: Ipv4Addr,
    pub client: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub lease_secs: u32,
}

/// The single lease this sink hands out: a /29 far from the usual home ranges.
pub const DEFAULT_LEASE: Lease = Lease {
    server: Ipv4Addr::new(192, 168, 173, 1),
    client: Ipv4Addr::new(192, 168, 173, 2),
    netmask: Ipv4Addr::new(255, 255, 255, 248),
    lease_secs: 3600,
};

pub fn parse(buf: &[u8]) -> Option<Request> {
    if buf.len() < OFF_OPTIONS || buf[0] != 1 || buf[OFF_MAGIC..OFF_OPTIONS] != MAGIC {
        return None;
    }
    let xid = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&buf[OFF_CHADDR..OFF_CHADDR + 6]);
    let mut kind = Kind::Other;
    let mut requested_ip = None;
    let mut client_id = None;
    let mut i = OFF_OPTIONS;
    while i + 2 <= buf.len() {
        let code = buf[i];
        if code == OPT_END {
            break;
        }
        if code == 0 {
            i += 1;
            continue;
        }
        let len = buf[i + 1] as usize;
        let value = buf.get(i + 2..i + 2 + len)?;
        match code {
            OPT_MESSAGE_TYPE if len == 1 => {
                kind = match value[0] {
                    1 => Kind::Discover,
                    3 => Kind::Request,
                    _ => Kind::Other,
                }
            }
            OPT_REQUESTED_IP if len == 4 => {
                requested_ip = Some(Ipv4Addr::new(value[0], value[1], value[2], value[3]))
            }
            OPT_CLIENT_ID if len > 0 => client_id = Some(value.to_vec()),
            _ => {}
        }
        i += 2 + len;
    }
    Some(Request {
        kind,
        xid,
        mac,
        requested_ip,
        client_id,
    })
}

/// Builds the reply for a request, or `None` when there is nothing to say.
pub fn reply(r: &Request, lease: &Lease) -> Option<Vec<u8>> {
    let message_type = match r.kind {
        Kind::Discover => 2, // OFFER
        Kind::Request => match r.requested_ip {
            // Asking for what we offered, or for nothing in particular.
            None => 5,
            Some(ip) if ip == lease.client => 5, // ACK
            Some(_) => 6,                        // NAK
        },
        Kind::Other => return None,
    };
    let mut p = vec![0u8; OFF_OPTIONS];
    p[0] = 2; // BOOTREPLY
    p[1] = 1; // ethernet
    p[2] = 6; // hardware address length
    p[4..8].copy_from_slice(&r.xid.to_be_bytes());
    if message_type != 6 {
        p[OFF_YIADDR..OFF_YIADDR + 4].copy_from_slice(&lease.client.octets());
    }
    p[OFF_SIADDR..OFF_SIADDR + 4].copy_from_slice(&lease.server.octets());
    p[OFF_CHADDR..OFF_CHADDR + 6].copy_from_slice(&r.mac);
    p[OFF_MAGIC..OFF_OPTIONS].copy_from_slice(&MAGIC);
    push_option(&mut p, OPT_MESSAGE_TYPE, &[message_type]);
    push_option(&mut p, OPT_SERVER_ID, &lease.server.octets());
    // Echoed unchanged, and on every message type including a NAK: a client
    // that sent one uses it to recognise the reply as its own.
    if let Some(id) = &r.client_id {
        push_option(&mut p, OPT_CLIENT_ID, id);
    }
    if message_type != 6 {
        push_option(&mut p, OPT_SUBNET, &lease.netmask.octets());
        push_option(&mut p, OPT_ROUTER, &lease.server.octets());
        push_option(&mut p, OPT_LEASE_TIME, &lease.lease_secs.to_be_bytes());
    }
    p.push(OPT_END);
    // BOOTP's minimum message is 300 octets and some clients still enforce it.
    // Padding after the end option costs nothing and cannot be misread.
    if p.len() < MIN_MESSAGE {
        p.resize(MIN_MESSAGE, 0);
    }
    Some(p)
}

fn push_option(p: &mut Vec<u8>, code: u8, value: &[u8]) {
    p.push(code);
    p.push(value.len() as u8);
    p.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const MAC: [u8; 6] = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

    fn packet(msg_type: u8, xid: u32, requested: Option<[u8; 4]>) -> Vec<u8> {
        packet_with_id(msg_type, xid, requested, None)
    }

    /// The client identifier Windows sends: option 61, type 1 then the MAC.
    fn windows_client_id() -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend_from_slice(&MAC);
        v
    }

    fn packet_with_id(
        msg_type: u8,
        xid: u32,
        requested: Option<[u8; 4]>,
        client_id: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut p = vec![0u8; 240];
        p[0] = 1; // BOOTREQUEST
        p[1] = 1; // ethernet
        p[2] = 6; // hardware length
        p[4..8].copy_from_slice(&xid.to_be_bytes());
        p[28..34].copy_from_slice(&MAC);
        p[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie
        p.extend_from_slice(&[53, 1, msg_type]);
        if let Some(ip) = requested {
            p.push(50);
            p.push(4);
            p.extend_from_slice(&ip);
        }
        if let Some(id) = client_id {
            p.push(61);
            p.push(id.len() as u8);
            p.extend_from_slice(id);
        }
        p.push(255);
        p
    }

    /// Finds a DHCP option in a reply and compares its value.
    fn has_option(buf: &[u8], code: u8, value: &[u8]) -> bool {
        let mut i = 240;
        while i + 2 <= buf.len() {
            let c = buf[i];
            if c == 255 {
                return false;
            }
            let len = buf[i + 1] as usize;
            if c == code {
                return &buf[i + 2..i + 2 + len] == value;
            }
            i += 2 + len;
        }
        false
    }

    #[test]
    fn a_reply_echoes_the_client_identifier_the_request_carried() {
        // RFC 6842: a server that received a client identifier must return it
        // unchanged. Windows sends one in every message and silently drops any
        // reply without it - which looks exactly like no server at all, and is
        // why a cast got its address offered and then fell back to APIPA.
        let id = windows_client_id();
        for msg_type in [1u8, 3u8] {
            let req = parse(&packet_with_id(
                msg_type,
                7,
                Some([192, 168, 173, 2]),
                Some(&id),
            ))
            .expect("parse");
            assert_eq!(req.client_id.as_deref(), Some(id.as_slice()));
            let reply = reply(&req, &DEFAULT_LEASE).expect("reply");
            assert!(
                has_option(&reply, 61, &id),
                "message type {msg_type} did not echo the client identifier"
            );
        }
    }

    #[test]
    fn a_nak_echoes_the_client_identifier_too() {
        // The client has to recognise a refusal as its own just as much.
        let id = windows_client_id();
        let req = parse(&packet_with_id(3, 7, Some([10, 0, 0, 9]), Some(&id))).expect("parse");
        let reply = reply(&req, &DEFAULT_LEASE).expect("reply");
        assert!(has_option(&reply, 53, &[6]), "expected a NAK");
        assert!(has_option(&reply, 61, &id));
    }

    #[test]
    fn a_request_without_a_client_identifier_gets_a_reply_without_one() {
        // Nothing is invented: only what the client sent comes back.
        let req = parse(&packet(1, 7, None)).expect("parse");
        assert_eq!(req.client_id, None);
        let reply = reply(&req, &DEFAULT_LEASE).expect("reply");
        assert!(!has_option(&reply, 61, &windows_client_id()));
    }

    #[test]
    fn a_reply_meets_the_minimum_message_length() {
        let req = parse(&packet(1, 7, None)).expect("parse");
        let reply = reply(&req, &DEFAULT_LEASE).expect("reply");
        assert!(reply.len() >= MIN_MESSAGE, "{} octets", reply.len());
    }

    #[test]
    fn a_discover_parses_with_its_transaction_and_mac() {
        let r = parse(&packet(1, 0xdeadbeef, None)).expect("parse");
        assert_eq!(r.kind, Kind::Discover);
        assert_eq!(r.xid, 0xdeadbeef);
        assert_eq!(r.mac, MAC);
        assert_eq!(r.requested_ip, None);
    }

    #[test]
    fn a_request_carries_the_address_it_wants() {
        let r = parse(&packet(3, 1, Some([192, 168, 173, 2]))).expect("parse");
        assert_eq!(r.kind, Kind::Request);
        assert_eq!(r.requested_ip, Some(Ipv4Addr::new(192, 168, 173, 2)));
    }

    #[test]
    fn a_packet_without_the_magic_cookie_is_rejected() {
        let mut p = packet(1, 1, None);
        p[236] = 0;
        assert!(parse(&p).is_none());
    }

    #[test]
    fn a_truncated_packet_is_rejected_without_panicking() {
        assert!(parse(&[1, 1, 6, 0]).is_none());
        let p = packet(1, 1, None);
        assert!(parse(&p[..100]).is_none());
    }

    #[test]
    fn a_discover_is_answered_with_an_offer_naming_our_addresses() {
        let r = parse(&packet(1, 7, None)).unwrap();
        let out = reply(&r, &DEFAULT_LEASE).expect("offer");
        assert_eq!(out[0], 2, "BOOTREPLY");
        assert_eq!(&out[4..8], &7u32.to_be_bytes(), "same transaction");
        assert_eq!(&out[16..20], &[192, 168, 173, 2], "your-address");
        assert_eq!(&out[28..34], &MAC);
        assert!(has_option(&out, 53, &[2]), "OFFER");
        assert!(
            has_option(&out, 54, &[192, 168, 173, 1]),
            "server identifier"
        );
        assert!(has_option(&out, 1, &[255, 255, 255, 248]), "/29 netmask");
        assert!(has_option(&out, 3, &[192, 168, 173, 1]), "router");
    }

    #[test]
    fn a_request_is_answered_with_an_ack() {
        let r = parse(&packet(3, 8, Some([192, 168, 173, 2]))).unwrap();
        let out = reply(&r, &DEFAULT_LEASE).expect("ack");
        assert!(has_option(&out, 53, &[5]), "ACK");
        assert!(has_option(&out, 51, &3600u32.to_be_bytes()), "lease time");
    }

    #[test]
    fn a_request_for_someone_elses_address_is_declined() {
        let r = parse(&packet(3, 9, Some([10, 0, 0, 5]))).unwrap();
        let out = reply(&r, &DEFAULT_LEASE).expect("nak");
        assert!(has_option(&out, 53, &[6]), "NAK");
    }

    #[test]
    fn any_other_message_type_is_ignored() {
        let r = parse(&packet(7, 10, None)).unwrap(); // RELEASE
        assert_eq!(r.kind, Kind::Other);
        assert!(reply(&r, &DEFAULT_LEASE).is_none());
    }
}
