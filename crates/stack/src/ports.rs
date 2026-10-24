//! Port specs, in the runtime's `[host-ip:][host-port:]container-port[/proto]`
//! form.
//!
//! The bracketed IPv6 form is the reason this is a parser rather than a
//! `split(':')`: `[::1]:8080:8000` has four colons and three fields.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec {
    /// The spec as written. This is what goes on the argv.
    pub source: String,
    pub host_ip: Option<IpAddr>,
    pub host_port: Option<u16>,
    pub container_port: u16,
    pub protocol: Protocol,
}

impl PortSpec {
    pub fn parse(input: &str) -> Result<Self, String> {
        if input.is_empty() {
            return Err("a port spec is required, such as `127.0.0.1:8184:8184`".to_owned());
        }

        let (body, protocol) = match input.rsplit_once('/') {
            Some((body, protocol)) => {
                let protocol = match protocol.to_ascii_lowercase().as_str() {
                    "tcp" => Protocol::Tcp,
                    "udp" => Protocol::Udp,
                    other => return Err(format!("unknown protocol `{other}`; use `tcp` or `udp`")),
                };
                (body, protocol)
            }
            None => (input, Protocol::Tcp),
        };

        let (bracketed_ip, fields) = if let Some(rest) = body.strip_prefix('[') {
            let close = rest
                .find(']')
                .ok_or_else(|| format!("`{input}`: an IPv6 address needs a closing `]`"))?;
            let address = &rest[..close];
            let after = &rest[close + 1..];
            let fields = after
                .strip_prefix(':')
                .ok_or_else(|| format!("`{input}`: expected `:port` after the IPv6 address"))?;
            let ip = IpAddr::from_str(address)
                .map_err(|_| format!("`{address}` is not an IP address"))?;
            (Some(ip), fields)
        } else {
            (None, body)
        };

        let parts: Vec<&str> = fields.split(':').collect();
        let (plain_ip, host_port, container_port) = match parts.as_slice() {
            [container] => (None, None, parse_port(container, input)?),
            [host, container] => (
                None,
                Some(parse_port(host, input)?),
                parse_port(container, input)?,
            ),
            [address, host, container] => {
                if bracketed_ip.is_some() {
                    return Err(format!(
                        "`{input}` has both a bracketed and a plain host address"
                    ));
                }
                let ip = IpAddr::from_str(address)
                    .map_err(|_| format!("`{address}` is not an IP address"))?;
                (
                    Some(ip),
                    Some(parse_port(host, input)?),
                    parse_port(container, input)?,
                )
            }
            _ => {
                return Err(format!(
                    "`{input}` is not `[host-ip:][host-port:]container-port[/tcp|/udp]`; \
                     IPv6 addresses need brackets, as in `[::1]:8080:8000`"
                ));
            }
        };

        Ok(Self {
            source: input.to_owned(),
            host_ip: bracketed_ip.or(plain_ip),
            host_port,
            container_port,
            protocol,
        })
    }

    /// The address and port this spec binds on the host, for duplicate
    /// detection. A spec without a host IP binds loopback, so it is normalised
    /// to `127.0.0.1` here.
    pub fn host_binding(&self) -> Option<(String, u16)> {
        let port = self.host_port?;
        let ip = self
            .host_ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "127.0.0.1".to_owned());
        Some((ip, port))
    }
}

fn parse_port(value: &str, whole: &str) -> Result<u16, String> {
    let port: u16 = value
        .parse()
        .map_err(|_| format!("`{whole}`: `{value}` is not a port number"))?;
    if port == 0 {
        return Err(format!("`{whole}`: port 0 is not a valid port"));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::{PortSpec, Protocol};

    #[test]
    fn parses_the_three_forms() {
        let container = PortSpec::parse("8184").unwrap();
        assert_eq!(container.host_port, None);
        assert_eq!(container.container_port, 8184);
        assert_eq!(container.protocol, Protocol::Tcp);

        let pair = PortSpec::parse("8080:80").unwrap();
        assert_eq!(pair.host_port, Some(8080));
        assert_eq!(pair.container_port, 80);

        let loopback = PortSpec::parse("127.0.0.1:8184:8184").unwrap();
        assert_eq!(loopback.host_ip.unwrap().to_string(), "127.0.0.1");
        assert_eq!(loopback.host_port, Some(8184));
    }

    #[test]
    fn parses_bracketed_ipv6() {
        let spec = PortSpec::parse("[::1]:8080:8000/udp").unwrap();
        assert_eq!(spec.host_ip.unwrap().to_string(), "::1");
        assert_eq!(spec.host_port, Some(8080));
        assert_eq!(spec.container_port, 8000);
        assert_eq!(spec.protocol, Protocol::Udp);
        assert_eq!(spec.host_binding().unwrap().0, "::1");
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(PortSpec::parse("127.0.0.1:8184:8184:1").is_err());
        assert!(PortSpec::parse("0:80").is_err());
        assert!(PortSpec::parse("8184/tcp/udp").is_err());
        assert!(PortSpec::parse("[::1:8184").is_err());
    }

    #[test]
    fn a_missing_host_ip_binds_loopback() {
        assert_eq!(
            PortSpec::parse("8184:8184").unwrap().host_binding(),
            Some(("127.0.0.1".to_owned(), 8184))
        );
        assert_eq!(PortSpec::parse("8184").unwrap().host_binding(), None);
    }
}
