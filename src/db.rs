//! SQLite persistence for evidence, immutable run records and the current
//! quantile cache.

use crate::model::{Evidence, EvidenceKind, Settings};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

pub struct Database {
    pub conn: Connection,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    payload TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence (
    id TEXT PRIMARY KEY,
    segment TEXT NOT NULL,
    kind TEXT NOT NULL,
    depth REAL NOT NULL,
    enabled INTEGER NOT NULL,
    mean REAL,
    sigma REAL,
    age_lo REAL,
    age_hi REAL,
    gap_end REAL,
    start_depth REAL,
    gap_mean REAL,
    gap_sigma REAL,
    label TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS runs (
    run_id TEXT PRIMARY KEY,
    feasible INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    record TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS quantiles (
    node_key TEXT NOT NULL,
    depth REAL NOT NULL,
    q_index INTEGER NOT NULL,
    q_value REAL NOT NULL,
    median REAL NOT NULL,
    source_run_id TEXT NOT NULL,
    PRIMARY KEY (node_key, q_index)
);
"#;

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Database> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        let mut db = Database { conn };
        if db.is_empty()? {
            crate::fixture::load_defaults(&mut db.conn)?;
        }
        Ok(db)
    }

    pub fn is_empty(&self) -> rusqlite::Result<bool> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM evidence", [], |r| r.get(0))?;
        Ok(count == 0)
    }

    pub fn load_fixture(&mut self) -> rusqlite::Result<()> {
        crate::fixture::load_defaults(&mut self.conn)
    }

    pub fn clear(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "DELETE FROM quantiles; DELETE FROM runs; DELETE FROM evidence; DELETE FROM settings;",
        )?;
        Ok(())
    }

    pub fn load_settings(&self) -> Settings {
        let row: rusqlite::Result<Option<String>> = self
            .conn
            .query_row("SELECT payload FROM settings WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional();
        match row {
            Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
            _ => Settings::default(),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> rusqlite::Result<()> {
        let json = serde_json::to_string(settings).expect("settings json");
        self.conn.execute(
            "INSERT INTO settings(id, payload) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
            [json],
        )?;
        Ok(())
    }

    pub fn list_evidence(&self) -> rusqlite::Result<Vec<Evidence>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, segment, kind, depth, enabled, mean, sigma, age_lo, age_hi,
                    gap_end, start_depth, gap_mean, gap_sigma, label
             FROM evidence ORDER BY depth, id",
        )?;
        let rows = stmt.query_map([], row_to_evidence)?;
        rows.collect()
    }

    pub fn get_evidence(&self, id: &str) -> rusqlite::Result<Option<Evidence>> {
        self.conn
            .query_row(
                "SELECT id, segment, kind, depth, enabled, mean, sigma, age_lo, age_hi,
                        gap_end, start_depth, gap_mean, gap_sigma, label
                 FROM evidence WHERE id = ?1",
                [id],
                row_to_evidence,
            )
            .optional()
    }

    pub fn upsert_evidence(&self, ev: &Evidence) -> rusqlite::Result<()> {
        Self::upsert_on(&self.conn, ev)
    }

    pub(crate) fn upsert_on(conn: &Connection, ev: &Evidence) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO evidence(id, segment, kind, depth, enabled, mean, sigma, age_lo, age_hi,
                                  gap_end, start_depth, gap_mean, gap_sigma, label)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(id) DO UPDATE SET
               segment=excluded.segment, kind=excluded.kind, depth=excluded.depth,
               enabled=excluded.enabled, mean=excluded.mean, sigma=excluded.sigma,
               age_lo=excluded.age_lo, age_hi=excluded.age_hi, gap_end=excluded.gap_end,
               start_depth=excluded.start_depth, gap_mean=excluded.gap_mean,
               gap_sigma=excluded.gap_sigma, label=excluded.label",
            rusqlite::params![
                ev.id,
                ev.segment,
                kind_str(ev.kind),
                ev.depth,
                ev.enabled as i64,
                ev.mean,
                ev.sigma,
                ev.age_lo,
                ev.age_hi,
                ev.gap_end,
                ev.start_depth,
                ev.gap_mean,
                ev.gap_sigma,
                ev.label,
            ],
        )?;
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "UPDATE evidence SET enabled = ?2 WHERE id = ?1",
            rusqlite::params![id, enabled as i64],
        )?;
        Ok(n > 0)
    }

    pub fn store_run(&self, record: &Value) -> rusqlite::Result<()> {
        let run_id = record["run_id"].as_str().unwrap();
        let feasible = record["feasible"].as_bool().unwrap() as i64;
        let created_at = record["created_at"].as_str().unwrap_or("");
        let body = serde_json::to_string(record).unwrap();
        self.conn.execute(
            "INSERT OR IGNORE INTO runs(run_id, feasible, created_at, record) VALUES (?1,?2,?3,?4)",
            rusqlite::params![run_id, feasible, created_at, body],
        )?;
        Ok(())
    }

    pub fn list_runs(&self) -> rusqlite::Result<Vec<Value>> {
        let mut stmt = self
            .conn
            .prepare("SELECT record FROM runs ORDER BY created_at DESC, run_id DESC")?;
        let rows = stmt.query_map([], |r| {
            let body: String = r.get(0)?;
            Ok(serde_json::from_str::<Value>(&body).unwrap())
        })?;
        rows.collect()
    }

    pub fn get_run(&self, run_id: &str) -> rusqlite::Result<Option<Value>> {
        self.conn
            .query_row("SELECT record FROM runs WHERE run_id = ?1", [run_id], |r| {
                let body: String = r.get(0)?;
                Ok(serde_json::from_str::<Value>(&body).unwrap())
            })
            .optional()
    }

    pub fn replace_quantiles(&self, record: &Value) -> rusqlite::Result<()> {
        let run_id = record["run_id"].as_str().unwrap();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch("DELETE FROM quantiles;")?;
        let nodes = record["nodes"].as_array().cloned().unwrap_or_default();
        let mut stmt = tx.prepare(
            "INSERT INTO quantiles(node_key, depth, q_index, q_value, median, source_run_id)
             VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        for node in nodes {
            let key = node["key"].as_str().unwrap();
            let depth = node["depth"].as_f64().unwrap();
            let median = node["median"].as_f64().unwrap();
            let values = node["quantiles"].as_array().cloned().unwrap_or_default();
            for (i, value) in values.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    key,
                    depth,
                    i as i64,
                    value.as_f64().unwrap(),
                    median,
                    run_id
                ])?;
            }
        }
        drop(stmt);
        tx.commit()?;
        Ok(())
    }

    pub fn invalidate_keys(&self, keys: &[String]) -> rusqlite::Result<usize> {
        let mut removed = 0usize;
        for key in keys {
            removed += self
                .conn
                .execute("DELETE FROM quantiles WHERE node_key = ?1", [key])?;
        }
        Ok(removed)
    }

    pub fn invalidate_all_quantiles(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("DELETE FROM quantiles;")?;
        Ok(())
    }

    pub fn quantile_cache(&self) -> rusqlite::Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_key, depth, q_index, q_value, median, source_run_id
             FROM quantiles ORDER BY depth, node_key, q_index",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(serde_json::json!({
                "node_key": r.get::<_, String>(0)?,
                "depth": r.get::<_, f64>(1)?,
                "q_index": r.get::<_, i64>(2)?,
                "q_value": r.get::<_, f64>(3)?,
                "median": r.get::<_, f64>(4)?,
                "source_run_id": r.get::<_, String>(5)?,
            }))
        })?;
        rows.collect()
    }

    pub fn import_bundle(&self, bundle: &Value, replace: bool) -> rusqlite::Result<ImportReport> {
        if replace {
            self.clear()?;
        }
        let mut runs_added = 0usize;
        let mut runs_kept = 0usize;
        if let Some(settings) = bundle.get("settings") {
            if let Ok(settings) = serde_json::from_value::<Settings>(settings.clone()) {
                self.save_settings(&settings)?;
            }
        }
        if let Some(evidence) = bundle["evidence"].as_array() {
            for value in evidence {
                if let Ok(ev) = serde_json::from_value::<Evidence>(value.clone()) {
                    self.upsert_evidence(&ev)?;
                }
            }
        }
        if let Some(runs) = bundle["runs"].as_array() {
            for record in runs {
                if self
                    .get_run(record["run_id"].as_str().unwrap_or(""))?
                    .is_some()
                {
                    runs_kept += 1;
                    continue;
                }
                self.store_run(record)?;
                runs_added += 1;
            }
        }
        if let Some(cache) = bundle["quantiles"].as_array() {
            for row in cache {
                self.conn.execute(
                    "INSERT OR REPLACE INTO quantiles
                     (node_key, depth, q_index, q_value, median, source_run_id)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![
                        row["node_key"].as_str().unwrap_or(""),
                        row["depth"].as_f64().unwrap_or(0.0),
                        row["q_index"].as_i64().unwrap_or(0),
                        row["q_value"].as_f64().unwrap_or(0.0),
                        row["median"].as_f64().unwrap_or(0.0),
                        row["source_run_id"].as_str().unwrap_or(""),
                    ],
                )?;
            }
        }
        Ok(ImportReport {
            runs_added,
            runs_kept,
        })
    }

    pub fn export_bundle(&self) -> rusqlite::Result<Value> {
        let settings = self.load_settings();
        let evidence = self.list_evidence()?;
        let runs = self.list_runs()?;
        let quantiles = self.quantile_cache()?;
        Ok(serde_json::json!({
            "bundle_schema": "hanleng-nianchi/bundle/v1",
            "exported_at": crate::time::now_iso(),
            "settings": settings,
            "evidence": evidence,
            "runs": runs,
            "quantiles": quantiles,
        }))
    }
}

pub struct ImportReport {
    pub runs_added: usize,
    pub runs_kept: usize,
}

pub fn kind_str(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Layer => "layer",
        EvidenceKind::Ash => "ash",
        EvidenceKind::Isotope => "isotope",
        EvidenceKind::Hard => "hard",
        EvidenceKind::Gap => "gap",
    }
}

pub fn parse_kind(value: &str) -> rusqlite::Result<EvidenceKind> {
    match value {
        "layer" => Ok(EvidenceKind::Layer),
        "ash" => Ok(EvidenceKind::Ash),
        "isotope" => Ok(EvidenceKind::Isotope),
        "hard" => Ok(EvidenceKind::Hard),
        "gap" => Ok(EvidenceKind::Gap),
        other => Err(rusqlite::Error::ToSqlConversionFailure(
            format!("未知约束类型 {}", other).into(),
        )),
    }
}

fn row_to_evidence(row: &rusqlite::Row<'_>) -> rusqlite::Result<Evidence> {
    Ok(Evidence {
        id: row.get(0)?,
        segment: row.get(1)?,
        kind: parse_kind(&row.get::<_, String>(2)?)?,
        depth: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        mean: row.get(5)?,
        sigma: row.get(6)?,
        age_lo: row.get(7)?,
        age_hi: row.get(8)?,
        gap_end: row.get(9)?,
        start_depth: row.get(10)?,
        gap_mean: row.get(11)?,
        gap_sigma: row.get(12)?,
        label: row.get(13)?,
    })
}
