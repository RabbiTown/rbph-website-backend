use serde::{Deserialize, Serializer};
use sqlx::types::time::OffsetDateTime;

pub fn format_offset_datetime(dt: &OffsetDateTime) -> String {
    let date = dt.date();
    let time = dt.time();
    let offset = dt.offset();

    let nanoseconds = time.nanosecond();
    let microseconds = nanoseconds / 1000;

    let offset_hours = offset.whole_hours();
    let offset_minutes = offset.whole_minutes() % 60;
    let offset_sign = if offset_hours < 0 || (offset_hours == 0 && offset_minutes < 0) {
        "-"
    } else {
        "+"
    };

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:06}{}{:02}:{:02}",
        date.year(),
        date.month() as u8,
        date.day(),
        time.hour(),
        time.minute(),
        time.second(),
        microseconds,
        offset_sign,
        offset_hours.abs(),
        offset_minutes.abs(),
    )
}

pub mod serialize_offset_datetime {
    use super::*;
    use serde::Deserializer;

    pub fn serialize<S>(dt: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_offset_datetime(dt))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
            .map_err(serde::de::Error::custom)
    }
}

pub mod serialize_option_offset_datetime {
    use super::*;
    use serde::Deserializer;

    pub fn serialize<S>(dt: &Option<OffsetDateTime>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match dt {
            Some(dt) => super::serialize_offset_datetime::serialize(dt, serializer),
            None => serializer.serialize_none(),
        }
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<OffsetDateTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|s| {
                OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}
