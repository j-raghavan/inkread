//! Tests for the daily quotation (#195).

use super::*;

/// Recompiling today's issue must give today's quote, not a new one each time. The core reads no
/// clock, so this has to come out of the date string itself.
#[test]
fn the_same_date_always_gives_the_same_quote() {
    for date in ["21 Aug 2026", "1 Jan 2027", ""] {
        let first = quote_for(date);
        for _ in 0..5 {
            assert_eq!(quote_for(date), first, "date {date:?} was not stable");
        }
    }
}

#[test]
fn different_dates_generally_give_different_quotes() {
    let dates: Vec<String> = (1..=28).map(|d| format!("{d} Aug 2026")).collect();
    let picked: std::collections::HashSet<_> =
        dates.iter().map(|d| quote_for(d).unwrap().text).collect();
    assert!(
        picked.len() > 1,
        "a month of dates produced one quote — selection is not varying"
    );
}

/// Every quote must be reachable, or part of a curated collection is dead weight.
#[test]
fn a_year_of_dates_reaches_the_whole_collection() {
    let mut seen = std::collections::HashSet::new();
    for month in [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ] {
        for day in 1..=28 {
            seen.insert(quote_for(&format!("{day} {month} 2026")).unwrap().text);
        }
    }
    assert_eq!(
        seen.len(),
        all().len(),
        "unreachable quotes: {:?}",
        all()
            .iter()
            .map(|q| q.text)
            .filter(|t| !seen.contains(t))
            .collect::<Vec<_>>()
    );
}

/// The curation rule, enforced rather than merely documented: an entry without a named work is an
/// unverifiable attribution, which is the exact failure mode #195 is about.
#[test]
fn every_quote_names_its_author_and_the_work_it_comes_from() {
    for q in all() {
        assert!(!q.text.trim().is_empty(), "empty quote text");
        assert!(
            !q.author.trim().is_empty(),
            "quote with no author: {:?}",
            q.text
        );
        assert!(
            !q.work.trim().is_empty(),
            "quote with no work — attribution cannot be checked: {:?}",
            q.text
        );
        // The work carries its publication year, which is what places it in the public domain.
        assert!(
            q.work.contains(|c: char| c.is_ascii_digit()),
            "work should carry its year: {:?}",
            q.work
        );
        assert!(
            !q.text.contains("placeholder"),
            "placeholder left in the collection: {:?}",
            q.text
        );
    }
}

/// A collection with duplicates wastes rotation slots and looks like a bug to a reader who sees the
/// same line twice in a week.
#[test]
fn the_collection_has_no_duplicates() {
    let texts: std::collections::HashSet<_> = all().iter().map(|q| q.text).collect();
    assert_eq!(texts.len(), all().len(), "duplicate quote text");
}

/// A contents page has room for a line, not a page.
#[test]
fn quotes_are_short_enough_for_a_cover() {
    for q in all() {
        assert!(
            q.text.chars().count() <= 200,
            "{} chars is too long for a cover: {:?}",
            q.text.chars().count(),
            q.text
        );
    }
}
