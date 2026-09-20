-- Provider labels belong to one account. Legacy manual labels without a
-- provider binding remain local global labels; automatic categories remain
-- local system labels. A provider label that was previously shared by name is
-- split without touching remote state.

CREATE TEMP TABLE label_scope_map (
  old_id INTEGER NOT NULL,
  account_id INTEGER NOT NULL,
  new_id INTEGER NOT NULL,
  PRIMARY KEY (old_id, account_id),
  UNIQUE (new_id)
);

INSERT INTO label_scope_map (old_id, account_id, new_id)
WITH owners AS (
  SELECT local_label_id AS old_id, account_id
    FROM gmail_labels
   WHERE local_label_id IS NOT NULL
  UNION
  SELECT ml.label_id AS old_id, m.account_id
    FROM message_labels ml
    JOIN messages m ON m.id = ml.message_id
   WHERE EXISTS (
     SELECT 1 FROM gmail_labels gl
      WHERE gl.local_label_id = ml.label_id
   )
),
ranked AS (
  SELECT old_id,
         account_id,
         MIN(account_id) OVER (PARTITION BY old_id) AS retained_account_id,
         ROW_NUMBER() OVER (ORDER BY old_id, account_id) AS sequence
    FROM owners
),
maximum AS (SELECT COALESCE(MAX(id), 0) AS id FROM labels)
SELECT old_id,
       account_id,
       CASE
         WHEN account_id = retained_account_id THEN old_id
         ELSE maximum.id + sequence
       END
  FROM ranked, maximum;

CREATE TABLE labels_v7 (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  color TEXT NOT NULL DEFAULT '#6b7280',
  keyword TEXT NOT NULL,
  position INTEGER NOT NULL DEFAULT 0,
  is_auto INTEGER NOT NULL DEFAULT 0,
  scope TEXT NOT NULL DEFAULT 'global'
    CHECK (scope IN ('account', 'global', 'automatic')),
  owner_account_id INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
  origin TEXT NOT NULL DEFAULT 'user'
    CHECK (origin IN ('provider', 'user', 'system')),
  CHECK (
    (scope = 'account' AND owner_account_id IS NOT NULL AND is_auto = 0)
    OR (scope = 'global' AND owner_account_id IS NULL AND is_auto = 0)
    OR (scope = 'automatic' AND owner_account_id IS NULL AND is_auto = 1)
  )
);

-- Retain each old id for automatic/global labels and for the first owner of a
-- provider label. This minimizes changes to saved local references.
INSERT INTO labels_v7 (
  id, name, color, keyword, position, is_auto, scope, owner_account_id, origin
)
SELECT l.id,
       COALESCE(
         (
           SELECT gl.name
             FROM gmail_labels gl
             JOIN label_scope_map m
               ON m.old_id = gl.local_label_id
              AND m.account_id = gl.account_id
            WHERE m.old_id = l.id AND m.new_id = l.id
            LIMIT 1
         ),
         l.name
       ),
       COALESCE(
         (
           SELECT gl.background_color
             FROM gmail_labels gl
             JOIN label_scope_map m
               ON m.old_id = gl.local_label_id
              AND m.account_id = gl.account_id
            WHERE m.old_id = l.id AND m.new_id = l.id
            LIMIT 1
         ),
         l.color
       ),
       l.keyword,
       l.position,
       l.is_auto,
       CASE
         WHEN l.is_auto = 1 THEN 'automatic'
         WHEN EXISTS (SELECT 1 FROM label_scope_map m WHERE m.old_id = l.id)
           THEN 'account'
         ELSE 'global'
       END,
       CASE
         WHEN l.is_auto = 0 THEN (
           SELECT m.account_id
             FROM label_scope_map m
            WHERE m.old_id = l.id AND m.new_id = l.id
         )
         ELSE NULL
       END,
       CASE
         WHEN l.is_auto = 1 THEN 'system'
         WHEN EXISTS (SELECT 1 FROM label_scope_map m WHERE m.old_id = l.id)
           THEN 'provider'
         ELSE 'user'
       END
  FROM labels l;

-- Add one independent row for every additional account that used a formerly
-- merged provider label. Names, colors, and keywords are copied locally only.
INSERT INTO labels_v7 (
  id, name, color, keyword, position, is_auto, scope, owner_account_id, origin
)
SELECT m.new_id,
       COALESCE(
         (
           SELECT gl.name
             FROM gmail_labels gl
            WHERE gl.local_label_id = m.old_id
              AND gl.account_id = m.account_id
            LIMIT 1
         ),
         l.name
       ),
       COALESCE(
         (
           SELECT gl.background_color
             FROM gmail_labels gl
            WHERE gl.local_label_id = m.old_id
              AND gl.account_id = m.account_id
            LIMIT 1
         ),
         l.color
       ),
       l.keyword,
       l.position,
       0,
       'account',
       m.account_id,
       'provider'
  FROM label_scope_map m
  JOIN labels l ON l.id = m.old_id
 WHERE m.new_id != m.old_id;

CREATE TABLE message_labels_v7 (
  message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  label_id INTEGER NOT NULL REFERENCES labels_v7(id) ON DELETE CASCADE,
  PRIMARY KEY (message_id, label_id)
);

INSERT INTO message_labels_v7 (message_id, label_id)
SELECT ml.message_id,
       COALESCE(m.new_id, ml.label_id)
  FROM message_labels ml
  JOIN messages message ON message.id = ml.message_id
  LEFT JOIN label_scope_map m
    ON m.old_id = ml.label_id AND m.account_id = message.account_id;

CREATE TABLE gmail_labels_v7 (
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  provider_id TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'system',
  folder_id INTEGER REFERENCES folders(id) ON DELETE SET NULL,
  local_label_id INTEGER REFERENCES labels_v7(id) ON DELETE SET NULL,
  background_color TEXT,
  text_color TEXT,
  PRIMARY KEY (account_id, provider_id)
);

INSERT INTO gmail_labels_v7 (
  account_id, provider_id, name, kind, folder_id, local_label_id,
  background_color, text_color
)
SELECT gl.account_id,
       gl.provider_id,
       gl.name,
       gl.kind,
       gl.folder_id,
       COALESCE(m.new_id, gl.local_label_id),
       gl.background_color,
       gl.text_color
  FROM gmail_labels gl
  LEFT JOIN label_scope_map m
    ON m.old_id = gl.local_label_id AND m.account_id = gl.account_id;

-- Queued offline label actions must keep the account-specific target after a
-- split. This is a local payload rewrite and performs no provider operation.
UPDATE pending_actions
   SET payload = json_set(
     payload,
     '$.labelId',
     (
       SELECT m.new_id
         FROM label_scope_map m
        WHERE m.old_id = json_extract(pending_actions.payload, '$.labelId')
          AND m.account_id = pending_actions.account_id
     )
   )
 WHERE kind IN ('add_label', 'remove_label')
   AND EXISTS (
     SELECT 1
       FROM label_scope_map m
      WHERE m.old_id = json_extract(pending_actions.payload, '$.labelId')
        AND m.account_id = pending_actions.account_id
   );

-- Label-based split queries already support "match any" arrays. Expand a
-- formerly shared provider label into every account-specific replacement so
-- the saved view keeps matching the same messages after the split.
UPDATE split_rules
   SET query_json = json_set(
     query_json,
     '$.labels',
     json((
       SELECT json_group_array(label_id)
         FROM (
           SELECT DISTINCT COALESCE(m.new_id, CAST(item.value AS INTEGER)) AS label_id
             FROM json_each(
                    CASE WHEN json_valid(split_rules.query_json)
                         THEN split_rules.query_json ELSE '{}' END,
                    '$.labels'
                  ) item
             LEFT JOIN label_scope_map m
               ON m.old_id = CAST(item.value AS INTEGER)
            ORDER BY label_id
         )
     ))
   )
 WHERE json_type(
         CASE WHEN json_valid(query_json) THEN query_json ELSE '{}' END,
         '$.labels'
       ) = 'array'
   AND EXISTS (
     SELECT 1
       FROM json_each(
              CASE WHEN json_valid(split_rules.query_json)
                   THEN split_rules.query_json ELSE '{}' END,
              '$.labels'
            ) item
       JOIN label_scope_map m ON m.old_id = CAST(item.value AS INTEGER)
   );

-- Automation rules have no account condition, so one old label action cannot
-- be rewritten as several account-owned actions without changing its meaning.
-- Preserve the rule and its prompt, but disable it for explicit user review.
WITH RECURSIVE
affected(rule_index) AS (
  SELECT DISTINCT CAST(rule.key AS INTEGER)
    FROM app_settings setting,
         json_each(
           CASE WHEN json_valid(setting.value) THEN setting.value ELSE '{}' END,
           '$.aiAutomationRules'
         ) rule,
         json_each(rule.value, '$.actions') action
    JOIN label_scope_map m
      ON m.old_id = CAST(json_extract(action.value, '$.value') AS INTEGER)
   WHERE setting.key = 'settings'
     AND json_extract(action.value, '$.kind') IN ('add_label', 'remove_label')
),
ordered(step, rule_index) AS (
  SELECT ROW_NUMBER() OVER (ORDER BY rule_index), rule_index FROM affected
),
patched(step, value) AS (
  SELECT 0, value FROM app_settings WHERE key = 'settings'
  UNION ALL
  SELECT patched.step + 1,
         json_set(
           patched.value,
           '$.aiAutomationRules[' || ordered.rule_index || '].enabled',
           json('false')
         )
    FROM patched
    JOIN ordered ON ordered.step = patched.step + 1
)
UPDATE app_settings
   SET value = (
     SELECT value FROM patched ORDER BY step DESC LIMIT 1
   )
 WHERE key = 'settings'
   AND EXISTS (SELECT 1 FROM affected);

-- Older Gmail actions could implicitly create a missing remote label while
-- replaying an add/remove. Keep the already-applied local membership, but do
-- not carry that surprising remote side effect across this migration.
DELETE FROM pending_actions
 WHERE kind IN ('add_label', 'remove_label')
   AND account_id IN (SELECT id FROM accounts WHERE provider = 'gmail')
   AND EXISTS (
     SELECT 1
       FROM labels_v7 l
      WHERE l.id = json_extract(pending_actions.payload, '$.labelId')
   )
   AND NOT EXISTS (
     SELECT 1
       FROM gmail_labels_v7 gl
      WHERE gl.account_id = pending_actions.account_id
        AND gl.local_label_id = json_extract(pending_actions.payload, '$.labelId')
   );

DROP TABLE message_labels;
DROP TABLE gmail_labels;
DROP TABLE labels;

ALTER TABLE labels_v7 RENAME TO labels;
ALTER TABLE message_labels_v7 RENAME TO message_labels;
ALTER TABLE gmail_labels_v7 RENAME TO gmail_labels;

CREATE INDEX idx_labels_owner ON labels(owner_account_id, position, name);
CREATE INDEX idx_message_labels_label ON message_labels(label_id, message_id);
CREATE INDEX idx_gmail_labels_local ON gmail_labels(local_label_id, account_id);

DROP TABLE label_scope_map;
