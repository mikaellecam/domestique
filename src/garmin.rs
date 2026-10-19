//! Garmin Connect, through the flow its mobile app uses: a browser sign-in
//! gives a service ticket, traded for OAuth2 tokens. It is undocumented and
//! Garmin can change it, or rotate the client id, without notice.

use crate::{Result, now, parse_utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

const SIGNIN_URL: &str = "https://sso.garmin.com/sso/signin";
const TOKEN_URL: &str = "https://diauth.garmin.com/di-oauth2-service/oauth/token";
const TICKET_GRANT: &str =
    "https://connectapi.garmin.com/di-oauth2-service/oauth/grant/service_ticket";
/// Public client id of the Garmin Connect Android app.
const CLIENT_ID: &str = "GARMIN_CONNECT_MOBILE_ANDROID_DI_2025Q2";
const API: &str = "https://connectapi.garmin.com";
/// Headers of the Garmin Connect Android app.
const APP_HEADERS: [(&str, &str); 7] = [
    ("User-Agent", "GCM-Android-5.23"),
    (
        "X-Garmin-User-Agent",
        "com.garmin.android.apps.connectmobile/5.23; ; Google/sdk_gphone64_arm64/google; Android/33; Dalvik/2.1.0",
    ),
    ("X-Garmin-Paired-App-Version", "10861"),
    ("X-Garmin-Client-Platform", "Android"),
    ("X-App-Ver", "10861"),
    ("X-Lang", "en"),
    ("X-GCExperience", "GC5"),
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    refresh_token: String,
    /// Unix time.
    pub expires_at: i64,
    /// Refresh tokens are bound to the client id that issued them.
    client_id: String,
}

/// Signs in on Garmin's own page, so this program never sees the password or
/// the MFA code. Garmin redirects the browser to a local port with a ticket.
/// `show_url` gets the page's address, for when no browser opens.
pub fn browser_login(show_url: impl FnOnce(&str)) -> Result<Tokens> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let callback = format!("http://{}/", listener.local_addr()?);
    let query: Vec<String> = [
        ("service", callback.as_str()),
        ("source", &callback),
        ("gauthHost", "https://sso.garmin.com/sso"),
        ("redirectAfterAccountLoginUrl", &callback),
        ("redirectAfterAccountCreationUrl", &callback),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", percent_encode(v)))
    .collect();
    let url = format!("{SIGNIN_URL}?{}", query.join("&"));
    show_url(&url);
    open_browser(&url);

    let ticket = wait_for_ticket(&listener)?;
    request_tokens(
        CLIENT_ID,
        &[
            ("client_id", CLIENT_ID),
            ("service_ticket", &ticket),
            ("grant_type", TICKET_GRANT),
            ("service_url", &callback),
        ],
    )
}

pub fn refresh(old: &Tokens) -> Result<Tokens> {
    let mut t = request_tokens(
        &old.client_id,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &old.client_id),
            ("refresh_token", &old.refresh_token),
        ],
    )
    .map_err(|e| format!("Garmin session expired: sign in to Garmin again ({e})"))?;
    // Garmin omits the refresh token when it does not rotate it.
    if t.refresh_token.is_empty() {
        t.refresh_token = old.refresh_token.clone();
    }
    Ok(t)
}

/// (start, end) in Unix time of the activities started at or after `since`.
pub fn activities_since(token: &str, since: i64) -> Result<Vec<(i64, i64)>> {
    let mut out = Vec::new();
    for start in (0..).step_by(100) {
        let page: Vec<Value> = api(
            ureq::get(format!(
                "{API}/activitylist-service/activities/search/activities"
            )),
            token,
        )
        .query("start", start.to_string())
        .query("limit", "100")
        .call()
        .map_err(|e| format!("Garmin activity list: {e}"))?
        .into_body()
        .read_json()?;
        for a in &page {
            let s = a["startTimeGMT"]
                .as_str()
                .and_then(parse_utc)
                .ok_or("Garmin activity without startTimeGMT")?;
            if s < since {
                return Ok(out);
            }
            out.push((s, s + a["duration"].as_f64().unwrap_or(0.0) as i64));
        }
        if page.len() < 100 {
            break;
        }
    }
    Ok(out)
}

/// Returns false when Garmin already has the activity.
pub fn upload(token: &str, name: &str, fit: &[u8]) -> Result<bool> {
    const BOUNDARY: &str = "whoosh2garmin-7f3a9c1e5b2d";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(fit);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let resp = api(ureq::post(format!("{API}/upload-service/upload")), token)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .send(&body[..]);
    let r: Value = match resp {
        Ok(r) => r.into_body().read_json().unwrap_or_default(),
        Err(ureq::Error::StatusCode(409)) => return Ok(false),
        Err(e) => return Err(format!("Garmin upload: {e}").into()),
    };
    let failures = &r["detailedImportResult"]["failures"];
    if failures.as_array().is_some_and(|f| !f.is_empty()) {
        return Err(format!("Garmin rejected {name}: {failures}").into());
    }
    Ok(true)
}

fn api<B>(mut req: ureq::RequestBuilder<B>, token: &str) -> ureq::RequestBuilder<B> {
    for (k, v) in APP_HEADERS {
        req = req.header(k, v);
    }
    req.header("Authorization", format!("Bearer {token}"))
}

fn request_tokens(client_id: &str, form: &[(&str, &str)]) -> Result<Tokens> {
    #[derive(Deserialize)]
    struct Response {
        access_token: String,
        #[serde(default)]
        refresh_token: String,
        expires_in: i64,
    }
    let mut req = ureq::post(TOKEN_URL)
        .header(
            "Authorization",
            format!("Basic {}", base64(&format!("{client_id}:"))),
        )
        .header("Accept", "application/json");
    for (k, v) in APP_HEADERS {
        req = req.header(k, v);
    }
    let r: Response = req
        .send_form(form.iter().copied())?
        .into_body()
        .read_json()?;
    Ok(Tokens {
        access_token: r.access_token,
        refresh_token: r.refresh_token,
        expires_at: now() + r.expires_in,
        client_id: client_id.to_owned(),
    })
}

/// Answers browser requests on the local port until one carries the ticket.
fn wait_for_ticket(listener: &TcpListener) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut line = String::new(); // "GET /?ticket=ST-... HTTP/1.1"
        BufReader::new(&stream).read_line(&mut line)?;
        let ticket = line
            .split_once("ticket=")
            .and_then(|(_, rest)| rest.split(['&', ' ']).next())
            .map(str::to_owned);
        let msg = match ticket {
            Some(_) => "Signed in to Garmin Connect. You can close this tab.",
            None => "Waiting for the Garmin sign-in.",
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
            msg.len()
        );
        if let Some(t) = ticket {
            return Ok(t);
        }
    }
    Err("local sign-in server stopped".into())
}

fn open_browser(url: &str) {
    let (cmd, args): (&str, &[&str]) = if cfg!(windows) {
        ("rundll32", &["url.dll,FileProtocolHandler"])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    let _ = std::process::Command::new(cmd).args(args).arg(url).spawn();
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn base64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    s.as_bytes()
        .chunks(3)
        .flat_map(|c| {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
            (0..4).map(move |i| match i <= c.len() {
                true => T[(n >> (18 - 6 * i) & 63) as usize] as char,
                false => '=',
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_reference() {
        // python3 -c 'import base64; print(base64.b64encode(b"..."))'
        assert_eq!(super::base64("ab"), "YWI=");
        assert_eq!(super::base64("abc:"), "YWJjOg==");
        assert_eq!(
            super::base64(&format!("{}:", super::CLIENT_ID)),
            "R0FSTUlOX0NPTk5FQ1RfTU9CSUxFX0FORFJPSURfRElfMjAyNVEyOg=="
        );
    }
}
