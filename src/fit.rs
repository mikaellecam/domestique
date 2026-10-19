//! Rewrites a MyWhoosh FIT file so Garmin Connect treats it as recorded by a
//! Garmin device: Garmin computes Training Effect and Load only for its own
//! devices.

use crate::Result;
use embedded_io_adapters::std::FromStd;
use rustyfit::profile::mesgdef::{FieldDescription, FileId, Lap, Record, Session};
use rustyfit::profile::typedef::{
    DateTime, GarminProduct, LapTrigger, Manufacturer, MesgNum, MessageIndex,
};
use rustyfit::proto::{Message, Value};
use rustyfit::{Decoder, Encoder};
use std::io::Cursor;

/// Device names as the FIT profile spells them, e.g. `edge_explore2`.
pub fn product_names() -> impl Iterator<Item = String> {
    (0..=u16::MAX)
        .map(|v| GarminProduct(v).to_string())
        .filter(|s| !s.starts_with("GarminProduct("))
}

pub fn product_id(name: &str) -> Option<u16> {
    (0..=u16::MAX).find(|&v| GarminProduct(v).to_string() == name)
}

pub fn rewrite(raw: &[u8], product: u16) -> Result<Vec<u8>> {
    let mut fit = decode(raw)?;

    for m in fit
        .messages
        .iter_mut()
        .filter(|m| m.num == MesgNum::FILE_ID)
    {
        set(
            m,
            FileId::MANUFACTURER,
            Value::Uint16(Manufacturer::GARMIN.0),
        );
        set(m, FileId::PRODUCT, Value::Uint16(product));
    }
    add_missing_last_lap(&mut fit.messages);

    encode(&mut fit)
}

/// Start (Unix time) and title of a MyWhoosh ride file.
pub fn summary(raw: &[u8]) -> Result<(i64, String)> {
    let fit = decode(raw)?;
    let ses = fit
        .messages
        .iter()
        .find(|m| m.num == MesgNum::SESSION)
        .ok_or("FIT file without a session")?;
    let start = Session::from(ses)
        .start_time
        .unix_timestamp()
        .ok_or("FIT session without a start time")?;
    // MyWhoosh stores the route name in a developer field called "Title".
    let title = fit
        .messages
        .iter()
        .filter(|m| m.num == MesgNum::FIELD_DESCRIPTION)
        .map(FieldDescription::from)
        .find(|d| d.field_name.first().is_some_and(|n| &**n == "Title"))
        .and_then(|d| {
            ses.developer_fields.iter().find(|f| {
                f.num == d.field_definition_number
                    && f.developer_data_index == d.developer_data_index
            })
        })
        .and_then(|f| match &f.value {
            Value::String(s) => Some(s.to_string()),
            _ => None,
        });
    Ok((start, title.unwrap_or_else(|| "MyWhoosh ride".to_owned())))
}

fn decode(raw: &[u8]) -> Result<rustyfit::proto::FIT> {
    // No component expansion: keep the fields MyWhoosh wrote and nothing more.
    Ok(Decoder::builder()
        .expand_components(false)
        .build()
        .decode(raw)
        .map_err(|e| format!("not a valid FIT file: {e:?}"))?
        .ok_or("empty FIT file")?)
}

fn encode(fit: &mut rustyfit::proto::FIT) -> Result<Vec<u8>> {
    // The encoder seeks back to patch the header, so it needs a cursor.
    let mut out = FromStd::new(Cursor::new(Vec::new()));
    Encoder::new()
        .encode(&mut out, fit)
        .map_err(|e| format!("encode FIT: {e:?}"))?;
    Ok(out.into_inner().into_inner())
}

/// MyWhoosh writes one lap per workout step, so a cool-down after the last
/// step belongs to no lap. Add a lap covering it, built from the records.
fn add_missing_last_lap(msgs: &mut Vec<Message>) {
    let Some(last_i) = msgs.iter().rposition(|m| m.num == MesgNum::LAP) else {
        return;
    };
    let Some(ses_i) = msgs.iter().position(|m| m.num == MesgNum::SESSION) else {
        return;
    };
    let last = Lap::from(&msgs[last_i]);
    let ses = Session::from(&msgs[ses_i]);
    let (start, end) = (
        last.timestamp.0,
        ses.start_time.0 + ses.total_elapsed_time / 1000,
    );
    if end <= start {
        return;
    }

    let recs: Vec<Record> = msgs
        .iter()
        .filter(|m| m.num == MesgNum::RECORD)
        .map(Record::from)
        .filter(|r| (start..end).contains(&r.timestamp.0))
        .collect();
    let laps_distance: u32 = msgs
        .iter()
        .filter(|m| m.num == MesgNum::LAP)
        .map(|m| Lap::from(m).total_distance)
        .sum();

    let mut lap = last.clone();
    lap.message_index = MessageIndex(last.message_index.0 + 1);
    lap.lap_trigger = LapTrigger::SESSION_END;
    lap.start_time = last.timestamp;
    lap.timestamp = DateTime(end);
    lap.total_elapsed_time = (end - start) * 1000;
    lap.total_timer_time = lap.total_elapsed_time;
    lap.total_distance = ses.total_distance.saturating_sub(laps_distance);
    lap.avg_speed = (lap.total_distance as u64 * 10 / (end - start) as u64) as u16; // cm/s -> mm/s
    lap.max_speed = max(recs.iter().map(|r| r.speed), u16::MAX);
    (lap.avg_power, lap.max_power) = avg_max(recs.iter().map(|r| r.power), u16::MAX);
    (lap.avg_heart_rate, lap.max_heart_rate) = avg_max(recs.iter().map(|r| r.heart_rate), u8::MAX);
    (lap.avg_cadence, lap.max_cadence) = avg_max(recs.iter().map(|r| r.cadence), u8::MAX);

    // Before the insert, which shifts the session's index.
    let num_laps = msgs.iter().filter(|m| m.num == MesgNum::LAP).count() as u16 + 1;
    set(&mut msgs[ses_i], Session::NUM_LAPS, Value::Uint16(num_laps));
    msgs.insert(last_i + 1, Message::from(lap));
}

fn set(m: &mut Message, num: u8, value: Value) {
    if let Some(f) = m.fields.iter_mut().find(|f| f.num == num) {
        f.value = value;
    }
}

/// Max of the valid values, or `invalid` when there are none.
fn max<T: Ord + Copy>(vals: impl Iterator<Item = T>, invalid: T) -> T {
    vals.filter(|&v| v != invalid).max().unwrap_or(invalid)
}

/// Truncated average and max of the valid values, as MyWhoosh computes them.
fn avg_max<T: Ord + Copy + Into<u64> + TryFrom<u64>>(
    vals: impl Iterator<Item = T> + Clone,
    invalid: T,
) -> (T, T) {
    let valid: Vec<u64> = vals
        .clone()
        .filter(|&v| v != invalid)
        .map(Into::into)
        .collect();
    if valid.is_empty() {
        return (invalid, invalid);
    }
    let avg = T::try_from(valid.iter().sum::<u64>() / valid.len() as u64).unwrap_or(invalid);
    (avg, max(vals, invalid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ride() -> Vec<u8> {
        let mut file_id = FileId::new();
        file_id.manufacturer = Manufacturer(331); // mywhoosh
        file_id.product = 3570;
        let mut msgs = vec![Message::from(file_id)];
        for t in 0..10u32 {
            let mut r = Record::new();
            r.timestamp = DateTime(1_000_000 + t);
            r.power = 100 + t as u16;
            r.heart_rate = 120;
            r.cadence = 80;
            r.speed = 5000;
            r.distance = t * 500;
            msgs.push(Message::from(r));
        }
        let mut lap = Lap::new();
        lap.message_index = MessageIndex(0);
        lap.start_time = DateTime(1_000_000);
        lap.timestamp = DateTime(1_000_006);
        lap.total_distance = 3000;
        msgs.push(Message::from(lap));
        let mut ses = Session::new();
        ses.start_time = DateTime(1_000_000);
        ses.total_elapsed_time = 10_000;
        ses.total_distance = 5000;
        ses.num_laps = 1;
        msgs.push(Message::from(ses));

        encode(&mut rustyfit::proto::FIT {
            messages: msgs,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn rewrite_sets_device_and_adds_missing_lap() {
        let out = rewrite(&ride(), GarminProduct::EDGE_EXPLORE2.0).unwrap();
        let fit = Decoder::new().decode(out.as_slice()).unwrap().unwrap();
        let get = |n| fit.messages.iter().filter(move |m| m.num == n);

        let file_id = FileId::from(get(MesgNum::FILE_ID).next().unwrap());
        assert_eq!(
            (file_id.manufacturer, file_id.product),
            (Manufacturer::GARMIN, 4169)
        );

        let laps: Vec<Lap> = get(MesgNum::LAP).map(Lap::from).collect();
        assert_eq!(laps.len(), 2);
        let tail = &laps[1];
        assert_eq!(
            (tail.start_time.0, tail.timestamp.0),
            (1_000_006, 1_000_010)
        );
        assert_eq!((tail.total_elapsed_time, tail.total_distance), (4000, 2000));
        assert_eq!((tail.avg_power, tail.max_power), (107, 109)); // records 6..=9
        assert_eq!(
            Session::from(get(MesgNum::SESSION).next().unwrap()).num_laps,
            2
        );
        assert_eq!(product_id("edge_explore2"), Some(4169));
        assert_eq!(
            summary(&out).unwrap(),
            (1_000_000 + 631_065_600, "MyWhoosh ride".to_owned())
        );
    }

    /// A real ride: RIDE=path/to/ride.fit cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_ride() {
        let raw = std::fs::read(std::env::var("RIDE").unwrap()).unwrap();
        println!("{:?}", summary(&raw).unwrap());
        rewrite(&raw, GarminProduct::EDGE_840.0).unwrap();
    }
}
