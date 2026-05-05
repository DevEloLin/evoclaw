use crate::{smart_format, Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::IpAddr;

#[derive(Deserialize)]
struct WebFetchArgs {
    url: String,
    #[serde(default)]
    max_chars: Option<usize>,
}

pub struct WebFetch;

/// Reject IPs that point at internal infra (loopback, RFC1918, link-local —
/// notably 169.254.169.254 cloud IMDS, 127.0.0.1 local services, 10/8, etc.).
fn is_disallowed_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_link_local() || v4.is_broadcast() {
                return true;
            }
            if v4.is_unspecified() || v4.is_multicast() {
                return true;
            }
            if v4.is_private() {
                return true;
            }
            // 100.64.0.0/10 (CGNAT) — also non-public.
            let octets = v4.octets();
            if octets[0] == 100 && (octets[1] & 0xC0) == 64 {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            let segs = v6.segments();
            // fe80::/10 — link-local.
            if (segs[0] & 0xFFC0) == 0xFE80 {
                return true;
            }
            // fc00::/7 — unique local.
            if (segs[0] & 0xFE00) == 0xFC00 {
                return true;
            }
            // ::ffff:0:0/96 — IPv4-mapped, re-check the embedded v4.
            if segs[0] == 0
                && segs[1] == 0
                && segs[2] == 0
                && segs[3] == 0
                && segs[4] == 0
                && segs[5] == 0xFFFF
            {
                let v4 = std::net::Ipv4Addr::new(
                    (segs[6] >> 8) as u8,
                    (segs[6] & 0xFF) as u8,
                    (segs[7] >> 8) as u8,
                    (segs[7] & 0xFF) as u8,
                );
                return is_disallowed_addr(IpAddr::V4(v4));
            }
            false
        }
    }
}

/// SSRF guard: parse `url`, resolve its host, and reject if any resolved
/// address falls into a disallowed range. URLs whose host is a literal IP are
/// checked directly. DNS happens here on purpose — the alternative (binding a
/// custom resolver into reqwest) is heavier and still requires this check.
async fn enforce_public_url(url: &str) -> Result<(), ToolError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|e| ToolError::InvalidArgs(format!("bad url: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| ToolError::Denied("url has no host".into()))?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(80);

    // Literal IP fast path.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_disallowed_addr(ip) {
            return Err(ToolError::Denied(format!(
                "url host resolves to internal address: {ip}"
            )));
        }
        return Ok(());
    }

    // DNS resolve and reject if any resolved IP is internal.
    let target = format!("{host}:{port}");
    let addrs = tokio::net::lookup_host(target.as_str())
        .await
        .map_err(|e| ToolError::Denied(format!("dns resolution failed: {e}")))?;
    let mut saw_any = false;
    for sa in addrs {
        saw_any = true;
        if is_disallowed_addr(sa.ip()) {
            return Err(ToolError::Denied(format!(
                "url host resolves to internal address: {}",
                sa.ip()
            )));
        }
    }
    if !saw_any {
        return Err(ToolError::Denied("dns returned no addresses".into()));
    }
    Ok(())
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch URL, return body. Cookie excluded from LLM."
    }
    fn permission(&self) -> Permission {
        Permission::P3
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" },
                "max_chars": { "type": "integer", "minimum": 100, "maximum": 100000 },
            },
            "required": ["url"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: WebFetchArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
            return Err(ToolError::Denied(
                "web_fetch only supports http(s) URLs".into(),
            ));
        }
        enforce_public_url(&a.url).await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let resp = client
            .get(&a.url)
            .send()
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let cap = a.max_chars.unwrap_or(8000);
        let truncated = smart_format(&body, cap);
        Ok(format!(
            "status={status}\ncontent-type={content_type}\n--- body ---\n{truncated}"
        ))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(WebFetch)
});

#[cfg(test)]
mod tests {
    use super::*;
    use evo_policy::Permission;
    use serde_json::json;

    #[tokio::test]
    async fn web_fetch_rejects_non_http() {
        let ctx = ToolContext::default().with_max_permission(Permission::P3);
        let err = WebFetch
            .run(&ctx, json!({"url": "ftp://example.com"}))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ToolError::Denied(_)));
    }

    #[tokio::test]
    async fn web_fetch_rejects_loopback() {
        let ctx = ToolContext::default().with_max_permission(Permission::P3);
        let err = WebFetch
            .run(&ctx, json!({"url": "http://127.0.0.1/secret"}))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn web_fetch_rejects_imds() {
        let ctx = ToolContext::default().with_max_permission(Permission::P3);
        let err = WebFetch
            .run(
                &ctx,
                json!({"url": "http://169.254.169.254/latest/meta-data/"}),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }
}
