use crate::error::Result;
use crate::models::{
    MessageCursor, MessageSearchCursor, MessageSearchPage, MessageSummary, SearchCursor,
    SearchPage, ThreadSummary, roles,
};
use crate::search::{Expr, Filter, ParsedQuery};
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// Index (or re-index) a message into the contentless FTS table.
pub fn index_message(conn: &Connection, message_id: i64) -> Result<()> {
    // Migration 013 enables contentless-delete mode, so this removes all old
    // terms without needing to reproduce the values that were indexed before
    // the message changed.
    conn.execute(
        "DELETE FROM messages_fts WHERE rowid = ?1",
        params![message_id],
    )?;
    conn.execute(
        "INSERT INTO messages_fts (rowid, subject, from_text, to_text, body)
         SELECT m.id,
                m.subject,
                COALESCE(m.from_name,'') || ' ' || COALESCE(m.from_addr,''),
                m.to_json || ' ' || m.cc_json,
                CASE
                  WHEN b.text_body IS NULL
                    OR TRIM(b.text_body, char(9) || char(10) || char(11) || char(12) || char(13) || char(32)) = ''
                  THEN COALESCE(m.snippet, '')
                  ELSE b.text_body
                END
         FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id
         WHERE m.id = ?1",
        params![message_id],
    )?;
    Ok(())
}

/// True when the query carries a real matcher (full-text terms or an operator),
/// as opposed to being empty or only account-scoped. Used to decide whether the
/// default Trash/Spam exclusion should apply.
fn has_positive_selector(q: &ParsedQuery) -> bool {
    !q.fts.is_empty()
        || q.filters.iter().any(|filter| *filter != Filter::Anywhere)
        || q.expression.as_ref().is_some_and(Expr::selects)
}

/// Append the caller's account scope over the `m` alias. It narrows a search
/// but is not a matcher by itself, so callers add it after deciding whether
/// the query matches anything.
fn append_account_clauses(
    q: &ParsedQuery,
    where_clauses: &mut Vec<String>,
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    if let Some(account_id) = q.account_id {
        bind.push(Box::new(account_id));
        where_clauses.push(format!("m.account_id = ?{}", bind.len()));
    }
    if let Some(account_ids) = &q.account_ids {
        where_clauses.push(account_set_clause("m.account_id", account_ids, bind));
    }
    if let Some(folder_ids) = &q.folder_ids {
        where_clauses.push(folder_set_clause(folder_ids, bind));
    }
}

/// Whether message `m` is in any of `folder_ids` as mail lists show it: its
/// folder, or for Gmail any of its labels. An empty set matches nothing.
pub(crate) fn folder_set_clause(
    folder_ids: &[i64],
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> String {
    if folder_ids.is_empty() {
        return "0".to_owned();
    }
    let placeholders = folder_ids
        .iter()
        .map(|folder_id| {
            bind.push(Box::new(*folder_id));
            format!("?{}", bind.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "(m.folder_id IN ({placeholders})
          OR (EXISTS (SELECT 1 FROM accounts ma
                      WHERE ma.id = m.account_id AND ma.provider = 'gmail')
              AND EXISTS (SELECT 1 FROM message_folders mf
                          WHERE mf.message_id = m.id AND mf.folder_id IN ({placeholders}))))"
    )
}

/// `column IN (…)` for a set of accounts; an empty set matches nothing.
pub(crate) fn account_set_clause(
    column: &str,
    account_ids: &[i64],
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> String {
    if account_ids.is_empty() {
        return "0".to_owned();
    }
    let placeholders = account_ids
        .iter()
        .map(|account_id| {
            bind.push(Box::new(*account_id));
            format!("?{}", bind.len())
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{column} IN ({placeholders})")
}

/// One operator's predicate over the `m` alias, binding its value. Placeholders
/// are `?n` keyed off the running `bind` length, so this composes with any
/// binds already pushed by the caller. `in:anywhere` keeps every message.
fn filter_clause(filter: &Filter, bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> String {
    match filter {
        Filter::From(from) => {
            bind.push(Box::new(format!("%{}%", from.to_lowercase())));
            format!(
                "(LOWER(COALESCE(m.from_addr,'')) LIKE ?{n} OR LOWER(COALESCE(m.from_name,'')) LIKE ?{n})",
                n = bind.len()
            )
        }
        Filter::To(to) => {
            bind.push(Box::new(format!("%{}%", to.to_lowercase())));
            format!("LOWER(m.to_json) LIKE ?{}", bind.len())
        }
        Filter::In(role) => {
            bind.push(Box::new(*role));
            format!(
                "EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = ?{})",
                bind.len()
            )
        }
        Filter::Anywhere => "1".to_owned(),
        Filter::Unread => "m.is_read = 0".to_owned(),
        Filter::Starred => "m.is_starred = 1".to_owned(),
        Filter::HasAttachment => "m.has_attachments = 1".to_owned(),
        Filter::Before(at) => {
            bind.push(Box::new(*at));
            format!("m.date < ?{}", bind.len())
        }
        Filter::After(at) => {
            bind.push(Box::new(*at));
            format!("m.date >= ?{}", bind.len())
        }
        // A message whose size isn't known matches neither.
        Filter::Larger(size) => {
            bind.push(Box::new(*size));
            format!("m.size >= ?{}", bind.len())
        }
        Filter::Smaller(size) => {
            bind.push(Box::new(*size));
            format!("m.size < ?{}", bind.len())
        }
        Filter::Filename(name) => {
            let escaped = name
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            bind.push(Box::new(format!("%{escaped}%")));
            format!(
                "EXISTS (SELECT 1 FROM attachments a WHERE a.message_id = m.id
                         AND LOWER(COALESCE(a.filename,'')) LIKE ?{} ESCAPE '\\')",
                bind.len()
            )
        }
    }
}

/// A boolean query's predicate over the `m` alias: words through the full-text
/// index, operators as in [`filter_clause`].
fn expression_clause(expression: &Expr, bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> String {
    let join =
        |parts: &[Expr], connector: &str, bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>| {
            let parts = parts
                .iter()
                .map(|part| expression_clause(part, bind))
                .collect::<Vec<_>>();
            format!("({})", parts.join(connector))
        };
    match expression {
        Expr::Text(text) => {
            bind.push(Box::new(text.fts.clone()));
            format!(
                "m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?{})",
                bind.len()
            )
        }
        Expr::Filter(filter) => filter_clause(filter, bind),
        Expr::Not(inner) => format!("NOT {}", expression_clause(inner, bind)),
        Expr::And(parts) => join(parts, " AND ", bind),
        Expr::Or(parts) => join(parts, " OR ", bind),
    }
}

/// Append the query's operators, or its boolean expression, as predicates over
/// the `m` alias.
fn append_operator_clauses(
    q: &ParsedQuery,
    where_clauses: &mut Vec<String>,
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    for filter in &q.filters {
        if *filter != Filter::Anywhere {
            let clause = filter_clause(filter, bind);
            where_clauses.push(clause);
        }
    }
    if let Some(expression) = &q.expression {
        let clause = expression_clause(expression, bind);
        where_clauses.push(clause);
    }
    if !q.names_folder() && has_positive_selector(q) && q.folder_ids.is_none() {
        // A folder scope names its folders, Trash and Spam only when meant.
        // No explicit `in:` scope: keep Trash and Spam out of results, the same
        // way the inbox and folder views do. Without this, a `from:`/keyword
        // search matches messages in every folder, so triaging a result (trash,
        // spam) leaves it matching the query and it reappears on the next
        // refetch. `in:trash` / `in:spam` / `in:anywhere` opt back in.
        // Gated on there being a real matcher so an empty query still returns
        // nothing (rather than every non-trash thread) via the caller's guard.
        where_clauses.push(outside_trash_and_spam());
    }
    // `exclude:`/`-term`: drop messages whose FTS content matches any excluded
    // term. Applied here (not in the FTS MATCH string) so it works uniformly
    // whether the message reached us via the lexical bm25 branch or the vector
    // branch, and even when there are no positive FTS terms to hang a `NOT` off.
    if !q.fts_not.is_empty() {
        bind.push(Box::new(q.fts_not.clone()));
        where_clauses.push(format!(
            "m.id NOT IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?{})",
            bind.len()
        ));
    }
}

/// Where a message has to match a chronological search: `matching`, the
/// query's own clauses, or for a message a server search found, the parts
/// this device can check without its body. Trash and Spam stay out unless
/// named. `None` when the query matches nothing.
fn match_clause(
    q: &ParsedQuery,
    matching: Vec<String>,
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Option<String> {
    let matching = (!matching.is_empty()).then(|| format!("({})", matching.join(" AND ")));
    if q.found.is_empty() {
        return matching;
    }
    let ids = q
        .found
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    // Ids come from our own database, so they're inlined rather than bound.
    let mut found = vec![format!("m.id IN ({ids})")];
    for filter in &q.filters {
        // What the server matched in text, attachments and file names needs
        // the body, but flags, folders, dates and sizes are kept here.
        if matches!(
            filter,
            Filter::Unread
                | Filter::Starred
                | Filter::In(_)
                | Filter::Before(_)
                | Filter::After(_)
                | Filter::Larger(_)
                | Filter::Smaller(_)
        ) {
            found.push(filter_clause(filter, bind));
        }
    }
    if !q.names_folder() && q.folder_ids.is_none() {
        found.push(outside_trash_and_spam());
    }
    let found = format!("({})", found.join(" AND "));
    Some(match matching {
        Some(matching) => format!("({matching} OR {found})"),
        None => found,
    })
}

fn outside_trash_and_spam() -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role IN ('{}','{}'))",
        roles::TRASH,
        roles::SPAM
    )
}

/// Lexical branch: thread ids ranked by bm25 + recency, capped at `cap`.
fn structured_thread_ids(conn: &Connection, q: &ParsedQuery, cap: i64) -> Result<Vec<i64>> {
    if cap <= 0 {
        return Ok(Vec::new());
    }

    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // bm25() may only be evaluated in a query directly over the FTS table, so
    // rank inside a subquery and join the results to messages. The ranked CTE must be MATERIALIZED:
    // if the planner flattens it into the outer join, bm25() loses its
    // full-text context and the query fails.
    let (cte, fts_join) = if q.fts.is_empty() {
        ("", "")
    } else {
        bind.push(Box::new(q.fts.clone()));
        (
            "WITH f AS MATERIALIZED (
                    SELECT rowid AS mid, bm25(messages_fts, 4.0, 2.0, 2.0, 1.0) AS fts_rank
                    FROM messages_fts WHERE messages_fts MATCH ?1
                    ORDER BY fts_rank LIMIT 2000)",
            "JOIN f ON f.mid = m.id",
        )
    };

    append_operator_clauses(q, &mut where_clauses, &mut bind);

    if where_clauses.is_empty() && q.fts.is_empty() {
        return Ok(Vec::new());
    }
    append_account_clauses(q, &mut where_clauses, &mut bind);

    let rank_expr = if q.fts.is_empty() {
        "0.0"
    } else {
        "f.fts_rank"
    };
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("AND {}", where_clauses.join(" AND "))
    };
    let sql = format!(
        "{cte}
             SELECT m.thread_id, MIN({rank_expr}) AS rank, MAX(m.date) AS d
             FROM messages m {fts_join}
             WHERE m.thread_id IS NOT NULL {where_sql}
             GROUP BY m.thread_id
             ORDER BY rank ASC, d DESC
             LIMIT {cap}"
    );

    // prepare_cached: the SQL text repeats across keystrokes (only binds
    // change), so skip re-planning on every call.
    let mut stmt = conn.prepare_cached(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(params_ref.as_slice())?;
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.next()? {
        let id = row.get::<_, i64>(0)?;
        if seen.insert(id) {
            ids.push(id);
            if ids.len() >= cap as usize {
                break;
            }
        }
    }
    Ok(ids)
}

/// Lexical branch at message granularity: message ids ranked by bm25 + recency
/// (or recency alone when operators-only), best first, capped. Mirrors
/// `structured_thread_ids` but returns individual messages so RAG/agentic
/// callers can retrieve and cite specific emails.
fn structured_message_ids(conn: &Connection, q: &ParsedQuery, cap: i64) -> Result<Vec<i64>> {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let (cte, fts_join) = if q.fts.is_empty() {
        ("", "")
    } else {
        bind.push(Box::new(q.fts.clone()));
        (
            "WITH f AS MATERIALIZED (
                SELECT rowid AS mid, bm25(messages_fts, 4.0, 2.0, 2.0, 1.0) AS fts_rank
                FROM messages_fts WHERE messages_fts MATCH ?1
                ORDER BY fts_rank LIMIT 2000)",
            "JOIN f ON f.mid = m.id",
        )
    };

    append_operator_clauses(q, &mut where_clauses, &mut bind);

    if where_clauses.is_empty() && q.fts.is_empty() {
        return Ok(Vec::new());
    }
    append_account_clauses(q, &mut where_clauses, &mut bind);

    let rank_expr = if q.fts.is_empty() {
        "0.0"
    } else {
        "f.fts_rank"
    };
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("AND {}", where_clauses.join(" AND "))
    };

    let sql = format!(
        "{cte}
         SELECT m.id
         FROM messages m {fts_join}
         WHERE m.thread_id IS NOT NULL {where_sql}
         ORDER BY {rank_expr} ASC, m.date DESC
         LIMIT {cap}"
    );

    let mut stmt = conn.prepare_cached(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let ids = stmt
        .query_map(params_ref.as_slice(), |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// The subset of `ids` (input order preserved) whose messages satisfy the
/// query's operator filters. Used to constrain semantic vector hits by
/// from:/to:/is:/etc. before fusing.
fn allowed_message_ids(conn: &Connection, q: &ParsedQuery, ids: &[i64]) -> Result<Vec<i64>> {
    let allowed = allowed_message_threads(conn, q, ids)?;
    Ok(ids
        .iter()
        .copied()
        .filter(|id| allowed.contains_key(id))
        .collect())
}

/// Message-level hybrid retrieval for RAG / agentic tools: fuse operator-aware
/// lexical (bm25) ids with the operator-filtered semantic hits via RRF, best
/// first, capped at `limit`. `vec_hits` are (message_id, score) from the vector
/// index; pass an empty slice for lexical-only.
pub fn message_hits(
    conn: &Connection,
    q: &ParsedQuery,
    vec_hits: &[(i64, f32)],
    limit: i64,
) -> Result<Vec<i64>> {
    let lexical = structured_message_ids(conn, q, candidate_cap(limit) as i64)?;

    let sem_ids: Vec<i64> = vec_hits.iter().map(|(id, _)| *id).collect();
    let allowed: std::collections::HashSet<i64> = allowed_message_ids(conn, q, &sem_ids)?
        .into_iter()
        .collect();
    let semantic: Vec<i64> = sem_ids
        .into_iter()
        .filter(|id| allowed.contains(id))
        .collect();

    let lists: Vec<Vec<i64>> = [lexical, semantic]
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect();
    let fused = rrf(&lists);
    Ok(fused
        .into_iter()
        .take(limit.max(1) as usize)
        .map(|(id, _)| id)
        .collect())
}

/// How many candidate thread ids each branch feeds into fusion for a given
/// result `limit`.
pub fn candidate_cap(limit: i64) -> usize {
    (limit.max(1) as usize * 4).clamp(50, 400)
}

/// Lexical candidates for `hybrid`'s fusion: bm25-ranked thread ids, retrying
/// relaxed (OR) when the all-terms query finds nothing. Split out so callers
/// can run it concurrently with query embedding.
pub fn lexical_thread_ids(conn: &Connection, q: &ParsedQuery, cap: usize) -> Result<Vec<i64>> {
    let ids = structured_thread_ids(conn, q, cap as i64)?;
    if !ids.is_empty() || q.fts.is_empty() || q.fts_or == q.fts {
        return Ok(ids);
    }
    let mut relaxed = q.clone();
    relaxed.fts = q.fts_or.clone();
    structured_thread_ids(conn, &relaxed, cap as i64)
}

/// For a set of candidate message ids, return message_id -> thread_id for those
/// that satisfy the query's operator filters. Ids are trusted (from our own
/// vector index), so they are inlined rather than bound.
fn allowed_message_threads(
    conn: &Connection,
    q: &ParsedQuery,
    ids: &[i64],
) -> Result<HashMap<i64, i64>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    append_operator_clauses(q, &mut where_clauses, &mut bind);
    append_account_clauses(q, &mut where_clauses, &mut bind);

    let id_list = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("AND {}", where_clauses.join(" AND "))
    };
    let sql = format!(
        "SELECT m.id, m.thread_id FROM messages m
         WHERE m.thread_id IS NOT NULL AND m.id IN ({id_list}) {where_sql}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind.iter().map(|b| b.as_ref()).collect();
    let mut map = HashMap::new();
    let rows = stmt.query_map(params_ref.as_slice(), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (mid, tid) = row?;
        map.insert(mid, tid);
    }
    Ok(map)
}

/// Semantic branch: fold message-level vector hits (already score-sorted) into
/// an operator-filtered, best-first, de-duplicated thread-id list.
fn vector_thread_ids(
    conn: &Connection,
    q: &ParsedQuery,
    vec_hits: &[(i64, f32)],
    cap: usize,
) -> Result<Vec<i64>> {
    if vec_hits.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = vec_hits.iter().map(|(id, _)| *id).collect();
    let allowed = allowed_message_threads(conn, q, &ids)?;

    let mut seen = std::collections::HashSet::new();
    let mut threads = Vec::new();
    for (mid, _score) in vec_hits {
        if let Some(&tid) = allowed.get(mid)
            && seen.insert(tid)
        {
            threads.push(tid);
            if threads.len() >= cap {
                break;
            }
        }
    }
    Ok(threads)
}

/// Reciprocal Rank Fusion. Scale-free, so it merges bm25 and cosine rankings
/// without normalizing their incomparable score ranges.
fn rrf(lists: &[Vec<i64>]) -> Vec<(i64, f32)> {
    const K: f32 = 60.0;
    let mut score: HashMap<i64, f32> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *score.entry(*id).or_insert(0.0) += 1.0 / (K + rank as f32 + 1.0);
        }
    }
    let mut fused: Vec<(i64, f32)> = score.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

/// How strongly a sender matches the query's free-text terms, in [0, 1].
/// A term that starts a name word or the address local-part counts fully
/// ("be" → "Bé Dọn Dẹp", "be@group.vn"); a mere substring counts weakly
/// ("be" somewhere inside "noreply@bendover.com"). Averaged over all terms so
/// partial-query matches don't outrank full-query ones.
fn sender_match(terms: &[String], name: &str, addr: &str) -> f32 {
    if terms.is_empty() {
        return 0.0;
    }
    let name_f = crate::search::fold(name);
    let addr_f = crate::search::fold(addr);
    let local = addr_f.split('@').next().unwrap_or("");
    let mut total = 0.0f32;
    for t in terms {
        let t = t.as_str();
        total += if name_f.split_whitespace().any(|w| w.starts_with(t))
            || local.split(['.', '-', '_']).any(|w| w.starts_with(t))
        {
            1.0
        } else if name_f.contains(t) || addr_f.contains(t) {
            0.4
        } else {
            0.0
        };
    }
    total / terms.len() as f32
}

/// Add per-thread sender-match, sender-affinity and recency bonuses to fused
/// RRF scores, so mail *from* someone matching the query, from people the user
/// actually corresponds with, and fresher threads outrank equally-matching
/// noise. Bonus weights live on the RRF scale - a single-list top rank scores
/// 1/61 ≈ 0.016 - with sender-match the strongest: a full name/address match
/// (query "be" → sender "Bé Dọn Dẹp") beats a top-ranked body-only hit.
fn apply_personal_boosts(
    conn: &Connection,
    q: &ParsedQuery,
    scored: &mut [(i64, f32)],
    now_ms: i64,
) -> Result<()> {
    if scored.is_empty() {
        return Ok(());
    }
    let id_list = scored
        .iter()
        .map(|(id, _)| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT m.thread_id, m.date,
                COALESCE(m.from_name, ''), COALESCE(m.from_addr, ''),
                COALESCE(c.send_count * 3 + c.recv_count, 0)
         FROM messages m
         LEFT JOIN contacts c ON c.email = LOWER(COALESCE(m.from_addr, ''))
         WHERE m.thread_id IN ({id_list})"
    );
    let mut stmt = conn.prepare(&sql)?;
    // Per thread: max date, max affinity, best sender match over its messages.
    let mut info: HashMap<i64, (i64, i64, f32)> = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (tid, date_ms, from_name, from_addr, affinity) = row?;
        let m = sender_match(&q.terms, &from_name, &from_addr);
        let e = info.entry(tid).or_insert((i64::MIN, 0, 0.0));
        e.0 = e.0.max(date_ms);
        e.1 = e.1.max(affinity);
        e.2 = e.2.max(m);
    }
    for (tid, score) in scored.iter_mut() {
        let Some(&(date_ms, affinity, sender)) = info.get(tid) else {
            continue;
        };
        let affinity = affinity as f32;
        let age_days = ((now_ms - date_ms).max(0) as f32) / 86_400_000.0;
        *score += 0.020 * sender
            + 0.008 * (affinity / (affinity + 25.0))
            + 0.004 / (1.0 + age_days / 30.0);
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(())
}

/// Lexical-only search (FTS5 bm25 + operator filters), grouped into threads.
/// Retained for callers that don't have a vector index.
pub fn search(conn: &Connection, q: &ParsedQuery, limit: i64) -> Result<Vec<ThreadSummary>> {
    let thread_ids = structured_thread_ids(conn, q, limit)?;
    hydrate(conn, &thread_ids)
}

/// Timeline search for mail-list UIs. Matching semantics stay identical to
/// lexical search, but the newest matching thread is always presented first.
pub fn chronological(conn: &Connection, q: &ParsedQuery, limit: i64) -> Result<Vec<ThreadSummary>> {
    Ok(chronological_page(conn, q, None, limit)?.threads)
}

/// One bounded page of chronological matches. Scan newest-first from the
/// beginning so only each thread's newest matching message is considered.
/// This also excludes older matches from threads shown on previous pages.
pub fn chronological_page(
    conn: &Connection,
    q: &ParsedQuery,
    cursor: Option<SearchCursor>,
    limit: i64,
) -> Result<SearchPage> {
    if limit <= 0 {
        return Ok(SearchPage {
            threads: Vec::new(),
            next_cursor: None,
        });
    }
    let mut query = q.clone();
    if cursor.is_some_and(|cursor| cursor.relaxed) {
        query.fts = query.fts_or.clone();
    }
    let mut page = chronological_page_for_query(conn, &query, cursor, limit)?;
    if page.threads.is_empty() && cursor.is_none() && !q.fts.is_empty() && q.fts_or != q.fts {
        query.fts = query.fts_or.clone();
        page = chronological_page_for_query(conn, &query, None, limit)?;
        if let Some(next_cursor) = &mut page.next_cursor {
            next_cursor.relaxed = true;
        }
    }
    Ok(page)
}

/// The query's own clauses for a chronological search, and the join its
/// full-text match needs. With mail a server search found, which needn't
/// match here, the full-text match is a condition instead of a join.
fn chronological_matching(
    q: &ParsedQuery,
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> (&'static str, Vec<String>) {
    let mut matching = Vec::new();
    let fts_join = if q.fts.is_empty() {
        ""
    } else {
        bind.push(Box::new(q.fts.clone()));
        if q.found.is_empty() {
            matching.push(format!("messages_fts MATCH ?{}", bind.len()));
            "JOIN messages_fts ON messages_fts.rowid = m.id"
        } else {
            matching.push(format!(
                "m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?{})",
                bind.len()
            ));
            ""
        }
    };
    append_operator_clauses(q, &mut matching, bind);
    (fts_join, matching)
}

/// Only the first `limit + 1` distinct threads beyond the cursor are needed.
/// A thread's first occurrence is its newest matching message, so scanning
/// from the top makes the cursor safe even when older messages also match.
fn chronological_page_for_query(
    conn: &Connection,
    q: &ParsedQuery,
    cursor: Option<SearchCursor>,
    limit: i64,
) -> Result<SearchPage> {
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let (fts_join, matching) = chronological_matching(q, &mut bind);
    let Some(matches) = match_clause(q, matching, &mut bind) else {
        return Ok(SearchPage {
            threads: Vec::new(),
            next_cursor: None,
        });
    };
    let mut where_clauses = vec!["m.thread_id IS NOT NULL".to_owned(), matches];
    append_account_clauses(q, &mut where_clauses, &mut bind);

    let sql = format!(
        "SELECT m.thread_id, m.date FROM messages m {fts_join}
         WHERE {} ORDER BY m.date DESC, m.thread_id DESC",
        where_clauses.join(" AND "),
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        bind.iter().map(|value| value.as_ref()).collect();
    let mut rows = stmt.query(params_ref.as_slice())?;
    let page_size = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut matches = Vec::with_capacity(page_size.min(200) + 1);
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.next()? {
        let thread_id = row.get::<_, i64>(0)?;
        if !seen.insert(thread_id) {
            continue;
        }
        let match_at = row.get::<_, i64>(1)?;
        if cursor.is_some_and(|cursor| {
            match_at > cursor.last_match_at
                || (match_at == cursor.last_match_at && thread_id >= cursor.thread_id)
        }) {
            continue;
        }
        matches.push((thread_id, match_at));
        if matches.len() > page_size {
            break;
        }
    }
    let has_more = matches.len() > page_size;
    matches.truncate(page_size);
    let next_cursor = if has_more {
        matches
            .last()
            .map(|(thread_id, last_match_at)| SearchCursor {
                last_match_at: *last_match_at,
                thread_id: *thread_id,
                relaxed: cursor.is_some_and(|cursor| cursor.relaxed),
            })
    } else {
        None
    };
    let thread_ids = matches.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    Ok(SearchPage {
        threads: hydrate(conn, &thread_ids)?,
        next_cursor,
    })
}

/// One page of chronological matches as single messages, for mail lists that
/// do not group conversations. Matching is the same as `chronological_page`,
/// including falling back to any-term matching when every term matched none.
pub fn chronological_message_page(
    conn: &Connection,
    q: &ParsedQuery,
    cursor: Option<MessageSearchCursor>,
    limit: i64,
) -> Result<MessageSearchPage> {
    if limit <= 0 {
        return Ok(MessageSearchPage {
            messages: Vec::new(),
            next_cursor: None,
        });
    }
    let relaxed = cursor.is_some_and(|cursor| cursor.relaxed);
    let mut query = q.clone();
    if relaxed {
        query.fts = query.fts_or.clone();
    }
    let position = cursor.map(|cursor| cursor.position);
    let mut page = chronological_message_page_for_query(conn, &query, position, limit, relaxed)?;
    if page.messages.is_empty() && cursor.is_none() && !q.fts.is_empty() && q.fts_or != q.fts {
        query.fts = query.fts_or.clone();
        page = chronological_message_page_for_query(conn, &query, None, limit, true)?;
    }
    Ok(page)
}

/// Messages page with a keyset cursor in SQL: unlike conversations, no match
/// can hide behind an earlier one, so nothing has to be scanned twice.
fn chronological_message_page_for_query(
    conn: &Connection,
    q: &ParsedQuery,
    cursor: Option<MessageCursor>,
    limit: i64,
    relaxed: bool,
) -> Result<MessageSearchPage> {
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let (fts_join, matching) = chronological_matching(q, &mut bind);
    let Some(matches) = match_clause(q, matching, &mut bind) else {
        return Ok(MessageSearchPage {
            messages: Vec::new(),
            next_cursor: None,
        });
    };
    let mut where_clauses = vec!["m.thread_id IS NOT NULL".to_owned(), matches];
    append_account_clauses(q, &mut where_clauses, &mut bind);
    if let Some(cursor) = cursor {
        bind.push(Box::new(cursor.date));
        let date_parameter = bind.len();
        bind.push(Box::new(cursor.message_id));
        where_clauses.push(format!(
            "(m.date, m.id) < (?{date_parameter}, ?{})",
            bind.len()
        ));
    }
    let sql = format!(
        "SELECT m.id, m.date FROM messages m {fts_join}
         WHERE {} ORDER BY m.date DESC, m.id DESC LIMIT {}",
        where_clauses.join(" AND "),
        limit + 1,
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        bind.iter().map(|value| value.as_ref()).collect();
    let mut matches = stmt
        .query_map(params_ref.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let page_size = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = if matches.len() > page_size {
        matches.truncate(page_size);
        matches
            .last()
            .map(|(message_id, date)| MessageSearchCursor {
                position: MessageCursor {
                    date: *date,
                    message_id: *message_id,
                },
                relaxed,
            })
    } else {
        None
    };
    let message_ids = matches.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
    Ok(MessageSearchPage {
        messages: super::threads::get_message_summaries(conn, &message_ids)?,
        next_cursor,
    })
}

/// Hybrid search: fuse the lexical (bm25) and semantic (vector KNN) branches
/// with RRF, preserving operator filters, and hydrate to thread summaries.
/// `vec_hits` are (message_id, score) from the in-memory index, score-sorted;
/// pass an empty slice to fall back to lexical-only ranking.
pub fn hybrid(
    conn: &Connection,
    q: &ParsedQuery,
    vec_hits: &[(i64, f32)],
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let lexical = lexical_thread_ids(conn, q, candidate_cap(limit))?;
    fuse(conn, q, lexical, vec_hits, limit)
}

/// Second half of `hybrid`: filter the semantic hits, fuse both candidate
/// lists with RRF + personal boosts, and hydrate the top threads. Takes
/// pre-computed lexical ids so the caller can overlap their query with the
/// (CPU-bound) query embedding.
pub fn fuse(
    conn: &Connection,
    q: &ParsedQuery,
    lexical: Vec<i64>,
    vec_hits: &[(i64, f32)],
    limit: i64,
) -> Result<Vec<ThreadSummary>> {
    let semantic = vector_thread_ids(conn, q, vec_hits, candidate_cap(limit))?;

    let lists: Vec<Vec<i64>> = [lexical, semantic]
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect();
    let mut fused = rrf(&lists);
    apply_personal_boosts(conn, q, &mut fused, chrono::Utc::now().timestamp_millis())?;

    let top: Vec<i64> = fused
        .into_iter()
        .take(limit.max(1) as usize)
        .map(|(id, _)| id)
        .collect();
    hydrate(conn, &top)
}

/// The conversations of messages a server search found, newest first by the
/// newest found message in each.
pub fn found_threads(conn: &Connection, message_ids: &[i64]) -> Result<Vec<ThreadSummary>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Ids come from our own database, so they're inlined rather than bound.
    let id_list = message_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT thread_id FROM messages
         WHERE id IN ({id_list}) AND thread_id IS NOT NULL
         GROUP BY thread_id
         ORDER BY MAX(date) DESC, thread_id DESC"
    ))?;
    let thread_ids = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    hydrate(conn, &thread_ids)
}

/// The messages a server search found, newest first, for lists that show
/// each message on its own.
pub fn found_messages(conn: &Connection, message_ids: &[i64]) -> Result<Vec<MessageSummary>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    let id_list = message_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM messages
         WHERE id IN ({id_list}) AND thread_id IS NOT NULL
         ORDER BY date DESC, id DESC"
    ))?;
    let ordered = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    super::threads::get_message_summaries(conn, &ordered)
}

/// How many conversations, or with `conversations` false how many messages,
/// have unread mail that `q` matches: a saved search's badge. As in the mail
/// list, a query that matches nothing with every word counts with any of them.
pub fn unread_matches(conn: &Connection, q: &ParsedQuery, conversations: bool) -> Result<i64> {
    let mut query = q.clone();
    if !q.fts.is_empty() && q.fts_or != q.fts && count_matches(conn, q, None)? == 0 {
        query.fts = query.fts_or.clone();
    }
    let counted = if conversations {
        "COUNT(DISTINCT m.thread_id)"
    } else {
        "COUNT(*)"
    };
    count_matches(conn, &query, Some(counted))
}

/// `counted` of the unread messages `q` matches, or without it whether `q`
/// matches any message at all, as 1 or 0.
fn count_matches(conn: &Connection, q: &ParsedQuery, counted: Option<&str>) -> Result<i64> {
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let (fts_join, matching) = chronological_matching(q, &mut bind);
    let Some(matches) = match_clause(q, matching, &mut bind) else {
        return Ok(0);
    };
    let mut where_clauses = vec!["m.thread_id IS NOT NULL".to_owned(), matches];
    if counted.is_some() {
        where_clauses.push("m.is_read = 0".to_owned());
    }
    append_account_clauses(q, &mut where_clauses, &mut bind);
    let found = format!(
        "FROM messages m {fts_join} WHERE {}",
        where_clauses.join(" AND ")
    );
    let sql = match counted {
        Some(counted) => format!("SELECT {counted} {found}"),
        None => format!("SELECT EXISTS(SELECT 1 {found})"),
    };
    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
        bind.iter().map(|value| value.as_ref()).collect();
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params_ref.as_slice(), |row| row.get(0))?)
}

fn hydrate(conn: &Connection, thread_ids: &[i64]) -> Result<Vec<ThreadSummary>> {
    // One IN query; get_summaries preserves the fused ranking order.
    super::threads::get_summaries(conn, thread_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{repo::messages, testutil};

    /// hydrate() runs one IN query; it must return summaries in the fused
    /// ranking order it was given, skipping ids that no longer exist.
    #[test]
    fn hydrate_preserves_ranking_order() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (t1, _) = testutil::seed_message(&c, "a@test.dev", "Alpha", false);
        let (t2, _) = testutil::seed_message(&c, "b@test.dev", "Beta", false);
        let (t3, _) = testutil::seed_message(&c, "c@test.dev", "Gamma", false);

        let order = [t2, t3, t1];
        let out = hydrate(&c, &order).unwrap();
        assert_eq!(out.iter().map(|t| t.id).collect::<Vec<_>>(), order);

        // Reversed input, reversed output; missing ids are skipped.
        let out = hydrate(&c, &[t1, 9999, t3, t2]).unwrap();
        assert_eq!(out.iter().map(|t| t.id).collect::<Vec<_>>(), [t1, t3, t2]);
    }

    #[test]
    fn trash_and_spam_are_hidden_unless_scoped() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        // A Trash folder to move the message into.
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (5,1,'Trash','trash')",
            [],
        )
        .unwrap();
        let (thread_id, msg_id) =
            testutil::seed_message(&c, "sender@test.dev", "Hello there", false);
        let parse = crate::search::parse;
        let ids = |q: &str| {
            search(&c, &parse(q), 20)
                .unwrap()
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };

        // In the inbox, a from: search finds it.
        assert_eq!(ids("from:sender"), vec![thread_id]);

        // Move it to Trash: the default search no longer returns it (so trashing
        // a result actually removes it from the list)...
        c.execute(
            "UPDATE messages SET folder_id = 5 WHERE id = ?1",
            params![msg_id],
        )
        .unwrap();
        assert!(ids("from:sender").is_empty());

        // ...but an explicit `in:trash` opts back in.
        assert_eq!(ids("in:trash from:sender"), vec![thread_id]);
    }

    #[test]
    fn dates_sizes_file_names_and_boolean_queries_filter_matches() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (old, old_message) = testutil::seed_message(&c, "ana@test.dev", "Budget 2023", false);
        let (new, new_message) = testutil::seed_message(&c, "ben@test.dev", "Budget 2026", false);
        let noon = |year| {
            chrono::NaiveDate::from_ymd_opt(year, 6, 1)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        };
        c.execute(
            "UPDATE messages SET date = ?1, size = 5000 WHERE id = ?2",
            params![noon(2023), old_message],
        )
        .unwrap();
        c.execute(
            "UPDATE messages SET date = ?1, size = 2000000 WHERE id = ?2",
            params![noon(2026), new_message],
        )
        .unwrap();
        c.execute(
            "INSERT INTO attachments (message_id, filename) VALUES (?1, 'Plan_2026.PDF')",
            params![new_message],
        )
        .unwrap();
        for message in [old_message, new_message] {
            index_message(&c, message).unwrap();
        }
        let ids = |query: &str| {
            chronological(&c, &crate::search::parse(query), 20)
                .unwrap()
                .iter()
                .map(|thread| thread.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(ids("budget after:2024/01/01"), vec![new]);
        assert_eq!(ids("budget before:2024/01/01"), vec![old]);
        assert_eq!(ids("larger:1M"), vec![new]);
        assert_eq!(ids("smaller:10K"), vec![old]);
        assert_eq!(ids("filename:pdf"), vec![new]);
        // An underscore in the name is itself, not any letter.
        assert_eq!(ids("filename:plan_2026"), vec![new]);
        assert!(ids("filename:plan_2023").is_empty());
        // Either sender, newest first.
        assert_eq!(ids("from:ana OR from:ben"), vec![new, old]);
        assert_eq!(ids("budget -from:ben"), vec![old]);
        assert_eq!(ids("(from:ana OR filename:pdf) budget"), vec![new, old]);
        assert!(ids("budget -(from:ana OR larger:1M)").is_empty());
        // Words in a boolean query still go through the full-text index.
        assert_eq!(ids("2023 OR zzzzzz"), vec![old]);
        assert!(ids("2023 zzzzzz OR yyyyyy").is_empty());
    }

    #[test]
    fn in_anywhere_and_boolean_folder_scopes_reach_trash() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (5,1,'Trash','trash')",
            [],
        )
        .unwrap();
        let (thread_id, message_id) =
            testutil::seed_message(&c, "sender@test.dev", "Hello there", false);
        c.execute(
            "UPDATE messages SET folder_id = 5 WHERE id = ?1",
            params![message_id],
        )
        .unwrap();
        let ids = |query: &str| {
            search(&c, &crate::search::parse(query), 20)
                .unwrap()
                .iter()
                .map(|thread| thread.id)
                .collect::<Vec<_>>()
        };
        assert!(ids("from:sender").is_empty());
        // A boolean query without a folder leaves Trash out too.
        assert!(ids("from:sender OR from:nobody").is_empty());
        assert_eq!(ids("in:anywhere from:sender"), vec![thread_id]);
        assert_eq!(ids("in:trash OR in:spam"), vec![thread_id]);
    }

    #[test]
    fn subject_and_body_operators_scope_the_column() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        // Subject and body carry the two distinct words in opposite columns.
        let (thread_id, message_id) =
            testutil::seed_message(&c, "sender@test.dev", "quarterlyplan meeting", false);
        messages::store_body(
            &c,
            message_id,
            Some("budgetnumbers here"),
            None,
            None,
            false,
            Some("budgetnumbers here"),
        )
        .unwrap();
        index_message(&c, message_id).unwrap();

        let ids = |query: &str| lexical_thread_ids(&c, &crate::search::parse(query), 20).unwrap();
        // Column-scoped match hits when the word is in that column...
        assert_eq!(ids("subject:quarterlyplan"), vec![thread_id]);
        assert_eq!(ids("body:budgetnumbers"), vec![thread_id]);
        // ...and misses when the word lives in the other column.
        assert!(ids("subject:budgetnumbers").is_empty());
        assert!(ids("body:quarterlyplan").is_empty());
    }

    #[test]
    fn account_id_scopes_results_to_one_account() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        // Account 1: a thread matching "sharedterm".
        let (t1, m1) = testutil::seed_message(&c, "a@one.dev", "sharedterm alpha", false);
        index_message(&c, m1).unwrap();

        // Account 2 with its own inbox folder and a thread also matching "sharedterm".
        c.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username,
             incoming_host, incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             VALUES (2,'me2@test.dev','generic','password','me2','h',993,'h',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2,2,'INBOX','inbox')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
             VALUES (99, 2, 'sharedterm beta', 1, 1000)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO messages (thread_id, account_id, folder_id, uid, message_id, subject,
             from_addr, date, is_read, is_automated, is_draft, is_outgoing)
             VALUES (99, 2, 2, 99, 'mid-99', 'sharedterm beta', 'b@two.dev', 1000, 0, 0, 0, 0)",
            [],
        )
        .unwrap();
        let m2 = c.last_insert_rowid();
        index_message(&c, m2).unwrap();

        let ids = |account_id: Option<i64>| {
            let mut q = crate::search::parse("sharedterm");
            q.account_id = account_id;
            let mut out = lexical_thread_ids(&c, &q, 20).unwrap();
            out.sort();
            out
        };
        // No scope: both accounts' threads match.
        assert_eq!(ids(None), vec![t1, 99]);
        // Scoped: only the selected account's thread.
        assert_eq!(ids(Some(1)), vec![t1]);
        assert_eq!(ids(Some(2)), vec![99]);

        // A profile's account set scopes every search branch the same way.
        let set_ids = |account_ids: Vec<i64>| {
            let mut q = crate::search::parse("sharedterm");
            q.account_ids = Some(account_ids);
            let mut lexical = lexical_thread_ids(&c, &q, 20).unwrap();
            lexical.sort();
            let mut timeline = chronological(&c, &q, 20)
                .unwrap()
                .into_iter()
                .map(|thread| thread.id)
                .collect::<Vec<_>>();
            timeline.sort();
            assert_eq!(lexical, timeline);
            lexical
        };
        assert_eq!(set_ids(vec![2]), vec![99]);
        assert_eq!(set_ids(vec![1, 2]), vec![t1, 99]);
        assert_eq!(set_ids(Vec::new()), Vec::<i64>::new());
    }

    #[test]
    fn a_folder_scope_keeps_matches_to_those_folders() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        c.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Junk', 'spam')",
            [],
        )
        .unwrap();
        let (_, inbox) = testutil::seed_message(&c, "a@one.dev", "quarterly plans", false);
        let (_, junk) = testutil::seed_message(&c, "b@two.dev", "quarterly prize", false);
        c.execute("UPDATE messages SET folder_id = 2 WHERE id = ?1", [junk])
            .unwrap();
        index_message(&c, inbox).unwrap();
        index_message(&c, junk).unwrap();

        let found = |folder_ids: Vec<i64>| {
            let query = ParsedQuery {
                folder_ids: Some(folder_ids),
                ..crate::search::parse("quarterly")
            };
            chronological_message_page(&c, &query, None, 20)
                .unwrap()
                .messages
                .into_iter()
                .map(|message| message.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(found(vec![1]), [inbox]);
        assert_eq!(found(vec![2]), [junk]);
        assert!(found(Vec::new()).is_empty());
        // Gmail labels count, as they do in mail lists.
        c.execute(
            "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, 2)",
            [inbox],
        )
        .unwrap();
        assert_eq!(found(vec![2]), [junk]);
        c.execute(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail' WHERE id = 1",
            [],
        )
        .unwrap();
        let mut both = found(vec![2]);
        both.sort();
        assert_eq!(both, [inbox, junk]);
    }

    #[test]
    fn an_account_scope_alone_matches_nothing() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "a@one.dev", "anything", false);
        index_message(&c, message_id).unwrap();

        for scope in [
            ParsedQuery {
                account_id: Some(1),
                ..ParsedQuery::default()
            },
            ParsedQuery {
                account_ids: Some(vec![1]),
                ..ParsedQuery::default()
            },
        ] {
            assert!(lexical_thread_ids(&c, &scope, 20).unwrap().is_empty());
            assert!(chronological(&c, &scope, 20).unwrap().is_empty());
        }
    }

    #[test]
    fn reindex_removes_stale_contentless_fts_terms() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_, message_id) = testutil::seed_message(&c, "sender@test.dev", "Subject", false);
        messages::store_body(
            &c,
            message_id,
            Some("obsoleteuniqueterm"),
            None,
            None,
            false,
            Some("obsoleteuniqueterm"),
        )
        .unwrap();
        index_message(&c, message_id).unwrap();
        messages::store_body(
            &c,
            message_id,
            Some("replacementuniqueterm"),
            None,
            None,
            false,
            Some("replacementuniqueterm"),
        )
        .unwrap();
        index_message(&c, message_id).unwrap();

        let matches = |term: &str| {
            c.query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
                params![term],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(matches("obsoleteuniqueterm"), 0);
        assert_eq!(matches("replacementuniqueterm"), 1);
    }

    #[test]
    fn message_search_lists_every_matching_message_and_pages() {
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (thread_id, first) = testutil::seed_message(&c, "a@b.dev", "rosterplan draft", false);
        let (own_thread, reply) = testutil::seed_message(&c, "a@b.dev", "re: rosterplan", false);
        c.execute(
            "UPDATE messages SET thread_id = ?1, date = 2000, uid = 10000 + id WHERE id = ?2",
            params![thread_id, reply],
        )
        .unwrap();
        c.execute("DELETE FROM threads WHERE id = ?1", [own_thread])
            .unwrap();
        let (_, unrelated) = testutil::seed_message(&c, "a@b.dev", "lunch", false);
        for id in [first, reply, unrelated] {
            index_message(&c, id).unwrap();
        }
        let query = crate::search::parse("rosterplan");

        // The conversation matches once; its messages match one by one.
        assert_eq!(
            chronological_page(&c, &query, None, 10)
                .unwrap()
                .threads
                .len(),
            1
        );
        let page = chronological_message_page(&c, &query, None, 1).unwrap();
        assert_eq!(
            page.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
            [reply]
        );
        let rest = chronological_message_page(&c, &query, page.next_cursor, 1).unwrap();
        assert_eq!(
            rest.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
            [first]
        );
        assert!(rest.next_cursor.is_none());
    }
}
