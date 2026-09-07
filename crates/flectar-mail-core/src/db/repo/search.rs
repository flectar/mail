use crate::error::Result;
use crate::models::{ThreadSummary, roles};
use crate::search::ParsedQuery;
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
        || q.from.is_some()
        || q.to.is_some()
        || q.in_folder.is_some()
        || q.is_unread == Some(true)
        || q.is_starred == Some(true)
        || q.has_attachment == Some(true)
}

/// Append `from:`/`to:`/`in:`/`is:`/`has:` predicates over the `m` alias.
/// Placeholders are `?n` keyed off the running `bind` length, so this composes
/// with any binds already pushed by the caller.
fn append_operator_clauses(
    q: &ParsedQuery,
    where_clauses: &mut Vec<String>,
    bind: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) {
    if let Some(account_id) = q.account_id {
        bind.push(Box::new(account_id));
        where_clauses.push(format!("m.account_id = ?{}", bind.len()));
    }
    if let Some(from) = &q.from {
        bind.push(Box::new(format!("%{}%", from.to_lowercase())));
        where_clauses.push(format!(
            "(LOWER(COALESCE(m.from_addr,'')) LIKE ?{n} OR LOWER(COALESCE(m.from_name,'')) LIKE ?{n})",
            n = bind.len()
        ));
    }
    if let Some(to) = &q.to {
        bind.push(Box::new(format!("%{}%", to.to_lowercase())));
        where_clauses.push(format!("LOWER(m.to_json) LIKE ?{}", bind.len()));
    }
    if let Some(role) = &q.in_folder {
        bind.push(Box::new(role.clone()));
        where_clauses.push(format!(
            "EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role = ?{})",
            bind.len()
        ));
    } else if has_positive_selector(q) {
        // No explicit `in:` scope: keep Trash and Spam out of results, the same
        // way the inbox and folder views do. Without this, a `from:`/keyword
        // search matches messages in every folder, so triaging a result (trash,
        // spam) leaves it matching the query and it reappears on the next
        // refetch. `in:trash` / `in:spam` set `in_folder` and opt back in.
        // Gated on there being a real matcher so an empty query still returns
        // nothing (rather than every non-trash thread) via the caller's guard.
        where_clauses.push(format!(
            "NOT EXISTS (SELECT 1 FROM folders f WHERE f.id = m.folder_id AND f.role IN ('{}','{}'))",
            roles::TRASH,
            roles::SPAM
        ));
    }
    if q.is_unread == Some(true) {
        where_clauses.push("m.is_read = 0".into());
    }
    if q.is_starred == Some(true) {
        where_clauses.push("m.is_starred = 1".into());
    }
    if q.has_attachment == Some(true) {
        where_clauses.push("m.has_attachments = 1".into());
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

/// Lexical branch: thread ids ranked by bm25 + recency or ordered strictly by
/// newest match for timeline callers, capped at `cap`.
fn structured_thread_ids_ordered(
    conn: &Connection,
    q: &ParsedQuery,
    cap: i64,
    chronological: bool,
) -> Result<Vec<i64>> {
    if cap <= 0 {
        return Ok(Vec::new());
    }

    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // bm25() may only be evaluated in a query directly over the FTS table, so
    // rank inside a subquery and join the results to messages. Chronological
    // callers must retain every FTS match; applying the relevance candidate
    // cap first could hide a newer, weaker match. They can join FTS directly
    // because they do not evaluate bm25(). The ranked CTE must be MATERIALIZED:
    // if the planner flattens it into the outer join, bm25() loses its
    // full-text context and the query fails.
    let (cte, fts_join) = if q.fts.is_empty() {
        ("", "")
    } else {
        bind.push(Box::new(q.fts.clone()));
        if chronological {
            where_clauses.push("messages_fts MATCH ?1".into());
            ("", "JOIN messages_fts ON messages_fts.rowid = m.id")
        } else {
            (
                "WITH f AS MATERIALIZED (
                    SELECT rowid AS mid, bm25(messages_fts, 4.0, 2.0, 2.0, 1.0) AS fts_rank
                    FROM messages_fts WHERE messages_fts MATCH ?1
                    ORDER BY fts_rank LIMIT 2000)",
                "JOIN f ON f.mid = m.id",
            )
        }
    };

    append_operator_clauses(q, &mut where_clauses, &mut bind);

    if where_clauses.is_empty() && q.fts.is_empty() {
        return Ok(Vec::new());
    }

    let rank_expr = if q.fts.is_empty() || chronological {
        "0.0"
    } else {
        "f.fts_rank"
    };
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("AND {}", where_clauses.join(" AND "))
    };
    let sql = if chronological {
        // Walking matches newest-first and de-duplicating below avoids a
        // GROUP BY/MAX temporary table. The first occurrence of each thread
        // is necessarily its newest matching message.
        format!(
            "{cte}
             SELECT m.thread_id
             FROM messages m {fts_join}
             WHERE m.thread_id IS NOT NULL {where_sql}
             ORDER BY m.date DESC, m.thread_id DESC"
        )
    } else {
        format!(
            "{cte}
             SELECT m.thread_id, MIN({rank_expr}) AS rank, MAX(m.date) AS d
             FROM messages m {fts_join}
             WHERE m.thread_id IS NOT NULL {where_sql}
             GROUP BY m.thread_id
             ORDER BY rank ASC, d DESC
             LIMIT {cap}"
        )
    };

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

fn structured_thread_ids(conn: &Connection, q: &ParsedQuery, cap: i64) -> Result<Vec<i64>> {
    structured_thread_ids_ordered(conn, q, cap, false)
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
    let mut thread_ids = structured_thread_ids_ordered(conn, q, limit, true)?;
    if thread_ids.is_empty() && !q.fts.is_empty() && q.fts_or != q.fts {
        let mut relaxed = q.clone();
        relaxed.fts = q.fts_or.clone();
        thread_ids = structured_thread_ids_ordered(conn, &relaxed, limit, true)?;
    }
    hydrate(conn, &thread_ids)
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
            "INSERT INTO folders (id, account_id, imap_name, role) VALUES (5,1,'Trash','trash')",
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
             imap_host, imap_port, smtp_host, smtp_port, created_at)
             VALUES (2,'me2@test.dev','imap','password','me2','h',993,'h',587,0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO folders (id, account_id, imap_name, role) VALUES (2,2,'INBOX','inbox')",
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
}
