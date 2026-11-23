//! The SID Windows derives from a service's name, whether or not the service
//! is installed: `S-1-5-80-` followed by the SHA-1 of the upper-cased name
//! in UTF-16, as five little-endian 32-bit numbers. `sc.exe showsid` prints
//! the same. A service that runs under its virtual account
//! (`NT SERVICE\<name>`) has this SID as its token's user.
use sha1::{Digest, Sha1};

/// The service SID of the service called `name`.
pub fn service_sid(name: &str) -> String {
    let upper = name.to_uppercase();
    let bytes: Vec<u8> = upper.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let hash = Sha1::digest(&bytes);
    let mut sid = String::from("S-1-5-80");
    for part in hash.as_slice().as_chunks::<4>().0 {
        sid.push_str(&format!("-{}", u32::from_le_bytes(*part)));
    }
    sid
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `sc.exe showsid` printed on MiniAir in the run of task
    /// `f71179f5` (2026-11-22).
    const SEEN: [(&str, &str); 4] = [
        (
            "agentc-egress",
            "S-1-5-80-1897209490-1139552037-2514284598-2277624500-1663993593",
        ),
        (
            "agentc-publication",
            "S-1-5-80-2610527295-3093505124-3895824955-3994243965-1923114604",
        ),
        (
            "agentc-runner",
            "S-1-5-80-2505214551-3868131734-2396360996-2952680925-136825992",
        ),
        (
            "agentc-supervisor",
            "S-1-5-80-3306791472-2437056731-757402864-3670258214-3591831055",
        ),
    ];

    #[test]
    fn the_sids_are_the_ones_windows_printed() {
        for (name, sid) in SEEN {
            assert_eq!(service_sid(name), sid, "{name}");
        }
    }

    #[test]
    fn the_case_of_the_name_makes_no_difference() {
        assert_eq!(service_sid("AgentC-Runner"), service_sid("agentc-runner"));
    }
}
