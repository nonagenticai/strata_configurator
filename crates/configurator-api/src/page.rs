//! List paging (spec section 10.1): `Paged{items,next}`, `limit` default 50 / max 500, cursor `before`.
//!
//! Lists are newest first. `next` is an opaque cursor naming the last item returned; passing it back as
//! `before` returns the items that come after it in the list (older ones). `next` is null on the last page.
use serde::Serialize;
use uuid::Uuid;

use crate::error::ApiError;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 500;

/// One page of a list.
#[derive(Debug, Clone, Serialize)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

impl<T> Paged<T> {
    /// `rows` as fetched with [`fetch`]`(limit)`: when there is one row more than `limit`, the extra row is
    /// dropped and `next` is the cursor of the last row kept.
    pub fn new(mut rows: Vec<T>, limit: i64, cursor: impl Fn(&T) -> String) -> Self {
        let limit = usize::try_from(limit).unwrap_or(0);
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(cursor)
        } else {
            None
        };
        Paged { items: rows, next }
    }
}

/// The `limit` query value: absent → 50; anything but an integer from 1 to 500 → 400 `invalid_limit`.
pub fn limit(raw: Option<&str>) -> Result<i64, ApiError> {
    let Some(s) = raw else {
        return Ok(DEFAULT_LIMIT);
    };
    match s.parse::<i64>() {
        Ok(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        _ => Err(ApiError::BadRequest(
            "invalid_limit",
            format!("limit must be an integer from 1 to {MAX_LIMIT}"),
        )),
    }
}

/// How many rows to fetch for a page of `limit`: one more, to learn whether a next page exists.
pub fn fetch(limit: i64) -> i64 {
    limit + 1
}

fn invalid_cursor() -> ApiError {
    ApiError::BadRequest(
        "invalid_cursor",
        "before must be a cursor returned as next by this list".into(),
    )
}

/// The position of a row in a list ordered by `(created_at, id)` descending. `us` is `created_at` in
/// microseconds since the Unix epoch (exact: Postgres keeps microseconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeCursor {
    pub us: i64,
    pub id: Uuid,
}

impl TimeCursor {
    pub fn encode(us: i64, id: Uuid) -> String {
        format!("{us}.{id}")
    }

    /// The `before` query value: absent → `None`; not a cursor this module wrote → 400 `invalid_cursor`.
    pub fn decode(raw: Option<&str>) -> Result<Option<TimeCursor>, ApiError> {
        let Some(s) = raw else {
            return Ok(None);
        };
        let (us, id) = s.split_once('.').ok_or_else(invalid_cursor)?;
        let us = us.parse::<i64>().map_err(|_| invalid_cursor())?;
        let id = Uuid::parse_str(id).map_err(|_| invalid_cursor())?;
        Ok(Some(TimeCursor { us, id }))
    }

    /// `(us, id)` for binding as two nullable query parameters.
    pub fn split(c: Option<TimeCursor>) -> (Option<i64>, Option<Uuid>) {
        match c {
            Some(c) => (Some(c.us), Some(c.id)),
            None => (None, None),
        }
    }
}

/// A `before` cursor over a revision number (lists ordered by `rev` descending).
pub fn rev_cursor(raw: Option<&str>) -> Result<Option<i32>, ApiError> {
    match raw {
        None => Ok(None),
        Some(s) => s
            .parse::<i32>()
            .ok()
            .filter(|r| *r >= 1)
            .map(Some)
            .ok_or_else(invalid_cursor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_defaults_and_bounds() {
        assert_eq!(limit(None).unwrap(), 50);
        assert_eq!(limit(Some("1")).unwrap(), 1);
        assert_eq!(limit(Some("500")).unwrap(), 500);
        for bad in ["0", "-1", "501", "abc", "", "1.5"] {
            let e = limit(Some(bad)).unwrap_err();
            assert_eq!(e.status_and_code().1, "invalid_limit", "{bad:?}");
        }
    }

    #[test]
    fn paged_sets_next_only_when_more_rows_exist() {
        let p = Paged::new(vec![1, 2, 3], 2, |n| n.to_string());
        assert_eq!(p.items, vec![1, 2]);
        assert_eq!(p.next.as_deref(), Some("2"));
        let p = Paged::new(vec![1, 2], 2, |n| n.to_string());
        assert_eq!(p.items, vec![1, 2]);
        assert_eq!(p.next, None);
    }

    #[test]
    fn time_cursor_round_trips_and_rejects_garbage() {
        let id = Uuid::from_u128(7);
        let c = TimeCursor::encode(-12, id);
        assert_eq!(
            TimeCursor::decode(Some(c.as_str())).unwrap(),
            Some(TimeCursor { us: -12, id })
        );
        assert_eq!(TimeCursor::decode(None).unwrap(), None);
        for bad in [
            "",
            "x",
            "1.",
            "1.not-a-uuid",
            "a.00000000-0000-0000-0000-000000000007",
        ] {
            let e = TimeCursor::decode(Some(bad)).unwrap_err();
            assert_eq!(e.status_and_code().1, "invalid_cursor", "{bad:?}");
        }
        assert_eq!(rev_cursor(Some("3")).unwrap(), Some(3));
        assert!(rev_cursor(Some("0")).is_err());
        assert!(rev_cursor(Some("x")).is_err());
    }
}
