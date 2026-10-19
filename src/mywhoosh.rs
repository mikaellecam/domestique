//! MyWhoosh's private API, the one its apps use. It is undocumented and can
//! change without notice, so every endpoint lives here.

use crate::{Result, parse_utc};
use serde_json::{Value, json};
use std::hash::{BuildHasher, RandomState};

const LOGIN_URL: &str = "https://services.mywhoosh.com/http-service/api/login";
const ACTIVITIES_URL: &str = "https://service14.mywhoosh.com/v2/rider/profile/activities";
const DOWNLOAD_URL: &str = "https://service14.mywhoosh.com/v2/rider/profile/download-activity-file";

pub struct Session {
    token: String,
    whoosh_id: String,
}

pub struct Ride {
    pub title: String,
    /// As MyWhoosh sends it, for display.
    pub when: String,
    /// Unix time.
    pub start: i64,
    file_id: String,
}

impl Session {
    pub fn login(email: &str, password: &str) -> Result<Self> {
        let r: Value = ureq::post(LOGIN_URL)
            .send_json(json!({
                "Username": email,
                "Password": password,
                "Platform": "Android",
                "Action": 1001,
                "CorrelationId": uuid(),
                "DeviceId": uuid(),
                "Authorization": "",
            }))
            .map_err(|e| format!("MyWhoosh login: {e}"))?
            .into_body()
            .read_json()?;
        if r["Success"] != true {
            return Err(format!("MyWhoosh login failed: {}", r["Message"]).into());
        }
        Ok(Session {
            token: scalar(&r["AccessToken"]).ok_or("MyWhoosh login: no access token")?,
            whoosh_id: scalar(&r["WhooshId"]).ok_or("MyWhoosh login: no WhooshId")?,
        })
    }

    /// Rides started at or after `since` (Unix time), newest first.
    pub fn rides_since(&self, since: i64) -> Result<Vec<Ride>> {
        let mut rides = Vec::new();
        for page in 1.. {
            let r: Value = ureq::post(ACTIVITIES_URL)
                .header("Authorization", format!("Bearer {}", self.token))
                .send_json(json!({ "sortDate": "DESC", "page": page }))
                .map_err(|e| format!("MyWhoosh activities: {e}"))?
                .into_body()
                .read_json()?;
            for a in r["data"]["results"].as_array().into_iter().flatten() {
                let title = a["title"].as_str().unwrap_or("untitled").to_owned();
                let when = a["startDatetime"].as_str().unwrap_or_default().to_owned();
                let start = parse_utc(&when).or(a["date"].as_f64().map(|d| d as i64));
                let (Some(start), Some(file_id)) = (start, scalar(&a["activityFileId"])) else {
                    eprintln!("skipping MyWhoosh activity without a date or file: {title}");
                    continue;
                };
                if start < since {
                    return Ok(rides);
                }
                rides.push(Ride { title, when, start, file_id });
            }
            if page >= r["data"]["totalPages"].as_u64().unwrap_or(0) {
                break;
            }
        }
        Ok(rides)
    }

    pub fn download(&self, ride: &Ride) -> Result<Vec<u8>> {
        let r: Value = ureq::post(DOWNLOAD_URL)
            .header("Authorization", format!("Bearer {}", self.token))
            .send_json(json!({ "key": self.whoosh_id, "fileId": ride.file_id }))
            .map_err(|e| format!("MyWhoosh download: {e}"))?
            .into_body()
            .read_json()?;
        // The response holds a short-lived link to the file, not the file.
        let url = r["data"]
            .as_str()
            .filter(|u| u.starts_with("https://"))
            .ok_or_else(|| format!("MyWhoosh download: no link for {}: {r}", ride.title))?;
        Ok(ureq::get(url).call()?.into_body().read_to_vec()?)
    }
}

/// A JSON string or number as text: MyWhoosh sends ids as either.
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A random version 4 UUID; the login wants one per request and device.
fn uuid() -> String {
    let (a, b) = (RandomState::new().hash_one(0), RandomState::new().hash_one(1));
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        (b >> 48) & 0x3fff | 0x8000,
        b & 0xffff_ffff_ffff
    )
}
