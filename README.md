**This repo is now archived.**

See https://github.com/geph-official/sosistab2 for the replacement, which multiplexes over multiple transports simultaneously.

# Sosistab - an obfuscated datagram transport for horrible networks

[![](https://img.shields.io/crates/v/sosistab)](https://crates.io/crates/sosistab)
![](https://img.shields.io/crates/l/sosistab)

Sosistab is an unreliable, obfuscated datagram transport over UDP and TCP, designed to achieve high performance even in extremely bad networks. Sosistab can be used for applications like anti-censorship VPNs, reliable communication over radios, game networking, etc. It also comes with a QUIC-like multiplex protocol that implements multiple TCP-like reliable streams over the base sosistab layer. This multiplex protocol is ideal for applications requiring a mix of reliable and unreliable traffic. For example, VPNs might do signaling and authentication over reliable streams, while passing packets through unreliable datagrams.

Features:

- Strong, state-of-the-art (obfs4-like) obfuscation. Sosistab servers cannot be detected by active probing, and Sosistab traffic is reasonably indistinguishable from random. We also make a best-effort attempt at hiding side-channels through random padding.
- Strong yet lightweight authenticated encryption with chacha20-poly1305
- Deniable public-key encryption with triple-x25519, with servers having long-term public keys that must be provided out-of-band. Similar to decent encrypted transports like TLS and DTLS --- but not to the whole Shadowsocks/Vmess family of protocols --- different clients have different session keys and cannot spy on each other.
- Reed-Solomon error correction that targets a certain application packet loss level. Intelligent autotuning and dynamic batch sizes make performance much better than other FEC-based tools like udpspeeder. This lets Sosistab turns high-bandwidth, high-loss links to medium-bandwidth, low-loss links, which is generally much more useful.
- Avoids last-mile congestive collapse but works around lossy links. Shamelessly unfair in permanently congested WANs --- but that's really their problem, not yours. In any case, permanently congested WANs are observationally identical to lossy links, and any solution for the latter will cause unfairness in the former.
