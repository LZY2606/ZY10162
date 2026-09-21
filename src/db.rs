//! SQLite 存储：当前工作状态（芯段/锦标/交换组）与不可变运行记录。

use crate::model::*;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub seq: i64,
    pub kind: String, // feasible | rejected | baseline
    pub status: String,
    pub reason: String,
    pub seed: u64,
    pub draws: usize,
    pub snapshot: SolveInput,
    pub result_json: String,
    pub conflict_json: Option<String>,
    pub parent_seq: Option<i64>,
    pub rerun_json: Option<String>,
}

pub struct Store {
    pub conn: Connection,
}

impl Store {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Store { conn };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS segments (
                id TEXT PRIMARY KEY,
                top REAL NOT NULL,
                bottom REAL NOT NULL,
                present INTEGER NOT NULL,
                gap_min_years REAL,
                note TEXT NOT NULL,
                version INTEGER NOT NULL,
                ord INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS markers (
                id TEXT PRIMARY KEY,
                depth REAL NOT NULL,
                kind TEXT NOT NULL,
                hard INTEGER NOT NULL,
                lo REAL NOT NULL,
                hi REAL NOT NULL,
                mean REAL,
                sd REAL,
                alt_means TEXT NOT NULL,
                exchange_group TEXT,
                weight REAL NOT NULL,
                excluded INTEGER NOT NULL,
                note TEXT NOT NULL,
                version INTEGER NOT NULL,
                ord INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS exchanges (
                grp TEXT PRIMARY KEY,
                swapped INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS runs (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                reason TEXT NOT NULL,
                seed INTEGER NOT NULL,
                draws INTEGER NOT NULL,
                parent_seq INTEGER,
                snapshot TEXT NOT NULL,
                result_json TEXT NOT NULL,
                conflict_json TEXT,
                rerun_json TEXT,
                created_seq_note TEXT NOT NULL DEFAULT ''
            );
            "#,
        )?;
        Ok(())
    }

    pub fn is_seeded(&self) -> rusqlite::Result<bool> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM segments", [], |r| r.get(0))?;
        Ok(n > 0)
    }

    pub fn reset_to_fixture(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "DELETE FROM runs; DELETE FROM markers; DELETE FROM segments; DELETE FROM exchanges;",
        )?;
        let seg = crate::fixtures::seed_segments();
        for (i, s) in seg.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO segments (id,top,bottom,present,gap_min_years,note,version,ord)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    s.id,
                    s.top,
                    s.bottom,
                    s.present as i64,
                    s.gap_min_years,
                    s.note,
                    s.version,
                    i as i64
                ],
            )?;
        }
        for (i, m) in crate::fixtures::seed_markers().iter().enumerate() {
            insert_marker(&self.conn, m, i as i64)?;
        }
        for e in crate::fixtures::seed_exchanges() {
            self.conn.execute(
                "INSERT INTO exchanges (grp, swapped) VALUES (?1,?2)",
                params![e.group, e.swapped as i64],
            )?;
        }
        Ok(())
    }

    pub fn load_segments(&self) -> rusqlite::Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,top,bottom,present,gap_min_years,note,version FROM segments ORDER BY ord",
        )?;
        let rows = stmt.query_map([], row_segment)?;
        rows.collect()
    }

    pub fn load_markers(&self) -> rusqlite::Result<Vec<Marker>> {
        let mut stmt = self
            .conn
            .prepare(concat!(
                "SELECT id,depth,kind,hard,lo,hi,mean,sd,alt_means,",
                "exchange_group,weight,excluded,note,version FROM markers ORDER BY ord"
            ))?;
        let rows = stmt.query_map([], row_marker)?;
        rows.collect()
    }

    pub fn load_exchanges(&self) -> rusqlite::Result<Vec<ExchangeState>> {
        let mut stmt = self
            .conn
            .prepare("SELECT grp,swapped FROM exchanges ORDER BY grp")?;
        let rows = stmt.query_map([], |r| {
            Ok(ExchangeState {
                group: r.get(0)?,
                swapped: r.get::<_, i64>(1)? != 0,
            })
        })?;
        rows.collect()
    }

    pub fn upsert_segment(&self, s: &Segment) -> rusqlite::Result<()> {
        validate_segments(&[s.clone()]).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, e.0))))?;
        self.conn.execute(
            "INSERT INTO segments (id,top,bottom,present,gap_min_years,note,version,ord)
             VALUES (?1,?2,?3,?4,?5,?6,?7, COALESCE((SELECT ord FROM segments WHERE id=?1),0))
             ON CONFLICT(id) DO UPDATE SET
               top=excluded.top, bottom=excluded.bottom, present=excluded.present,
               gap_min_years=excluded.gap_min_years, note=excluded.note,
               version=excluded.version",
            params![
                s.id,
                s.top,
                s.bottom,
                s.present as i64,
                s.gap_min_years,
                s.note,
                s.version
            ],
        )?;
        // 重新编号
        reorder(&self.conn, "segments")?;
        Ok(())
    }

    pub fn upsert_marker(&self, m: &Marker) -> rusqlite::Result<()> {
        m.validate().map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidInput, e.0))))?;
        let exists: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM markers WHERE id=?1",
                params![m.id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let ord = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(ord)+1,0) FROM markers",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if exists == 0 {
            insert_marker(&self.conn, m, ord)?;
        } else {
            self.conn
                .execute(
                    concat!(
                        "UPDATE markers SET depth=?2,kind=?3,hard=?4,lo=?5,hi=?6,mean=?7,sd=?8,",
                        "alt_means=?9,exchange_group=?10,weight=?11,excluded=?12,note=?13,version=?14",
                        " WHERE id=?1"
                    ),
                    params![
                        m.id,
                        m.depth,
                        m.kind,
                        m.hard as i64,
                        m.lo,
                        m.hi,
                        m.mean,
                        m.sd,
                        serde_json::to_string(&m.alt_means).unwrap(),
                        m.exchange_group,
                        m.weight,
                        m.excluded as i64,
                        m.note,
                        m.version
                    ],
                )?;
        }
        Ok(())
    }

    pub fn set_exchange(&self, group: &str, swapped: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO exchanges (grp,swapped) VALUES (?1,?2)
             ON CONFLICT(grp) DO UPDATE SET swapped=excluded.swapped",
            params![group, swapped as i64],
        )?;
        Ok(())
    }

    pub fn current_input(&self, seed: u64, draws: usize) -> rusqlite::Result<SolveInput> {
        Ok(SolveInput {
            seed,
            draws,
            solver_version: SOLVER_VERSION,
            segments: self.load_segments()?,
            markers: self.load_markers()?,
            exchanges: self.load_exchanges()?,
        })
    }

    pub fn insert_run(&self, rec: &NewRun) -> rusqlite::Result<i64> {
        self.conn.execute(
            concat!(
                "INSERT INTO runs (kind,status,reason,seed,draws,parent_seq,",
                "snapshot,result_json,conflict_json,rerun_json)",
                " VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
            ),
            params![
                rec.kind,
                rec.status,
                rec.reason,
                rec.seed as i64,
                rec.draws as i64,
                rec.parent_seq,
                serde_json::to_string(&rec.snapshot).unwrap(),
                rec.result_json,
                rec.conflict_json,
                rec.rerun_json
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_runs(&self) -> rusqlite::Result<Vec<RunRecord>> {
        let mut stmt = self.conn.prepare(
            concat!(
                "SELECT seq,kind,status,reason,seed,draws,snapshot,result_json,",
                "conflict_json,parent_seq,rerun_json FROM runs ORDER BY seq"
            ),
        )?;
        let rows = stmt.query_map([], row_run)?;
        rows.collect()
    }

    pub fn get_run(&self, seq: i64) -> rusqlite::Result<Option<RunRecord>> {
        let mut stmt = self.conn.prepare(
            concat!(
                "SELECT seq,kind,status,reason,seed,draws,snapshot,result_json,",
                "conflict_json,parent_seq,rerun_json FROM runs WHERE seq=?1"
            ),
        )?;
        let mut rows = stmt.query_map(params![seq], row_run)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn clear_runs(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("DELETE FROM runs;")?;
        Ok(())
    }

    pub fn clear_all(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "DELETE FROM runs; DELETE FROM markers; DELETE FROM segments; DELETE FROM exchanges;",
        )?;
        Ok(())
    }

    pub fn import_bundle(&self, bundle: &crate::exchange::Bundle) -> rusqlite::Result<()> {
        self.clear_all()?;
        for (i, s) in bundle.segments.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO segments (id,top,bottom,present,gap_min_years,note,version,ord)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    s.id,
                    s.top,
                    s.bottom,
                    s.present as i64,
                    s.gap_min_years,
                    s.note,
                    s.version,
                    i as i64
                ],
            )?;
        }
        for (i, m) in bundle.markers.iter().enumerate() {
            insert_marker(&self.conn, m, i as i64)?;
        }
        for e in &bundle.exchanges {
            self.conn.execute(
                "INSERT INTO exchanges (grp,swapped) VALUES (?1,?2)",
                params![e.group, e.swapped as i64],
            )?;
        }
        for r in &bundle.runs {
            self.conn.execute(
                concat!(
                    "INSERT INTO runs (seq,kind,status,reason,seed,draws,parent_seq,",
                    "snapshot,result_json,conflict_json,rerun_json)",
                    " VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
                ),
                params![
                    r.seq,
                    r.kind,
                    r.status,
                    r.reason,
                    r.seed as i64,
                    r.draws as i64,
                    r.parent_seq,
                    serde_json::to_string(&r.snapshot).unwrap(),
                    r.result_json,
                    r.conflict_json,
                    r.rerun_json
                ],
            )?;
        }
        Ok(())
    }
}

pub struct NewRun<'a> {
    pub kind: &'a str,
    pub status: &'a str,
    pub reason: String,
    pub seed: u64,
    pub draws: usize,
    pub snapshot: &'a SolveInput,
    pub result_json: String,
    pub conflict_json: Option<String>,
    pub parent_seq: Option<i64>,
    pub rerun_json: Option<String>,
}

fn insert_marker(conn: &Connection, m: &Marker, ord: i64) -> rusqlite::Result<()> {
    conn.execute(
        concat!(
            "INSERT INTO markers (id,depth,kind,hard,lo,hi,mean,sd,alt_means,",
            "exchange_group,weight,excluded,note,version,ord)",
            " VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"
        ),
        params![
            m.id,
            m.depth,
            m.kind,
            m.hard as i64,
            m.lo,
            m.hi,
            m.mean,
            m.sd,
            serde_json::to_string(&m.alt_means).unwrap(),
            m.exchange_group,
            m.weight,
            m.excluded as i64,
            m.note,
            m.version,
            ord
        ],
    )?;
    Ok(())
}

fn reorder(conn: &Connection, table: &str) -> rusqlite::Result<()> {
    // 按 top 排序重写 ord（仅 segments 用）。
    if table != "segments" {
        return Ok(());
    }
    let mut stmt = conn.prepare("UPDATE segments SET ord=?1 WHERE id=?2")?;
    let rows: Vec<(String, f64)> = {
        let mut q = conn.prepare("SELECT id,top FROM segments")?;
        let mapped = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut ordered = rows;
    ordered.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then_with(|| a.0.cmp(&b.0)));
    for (i, (id, _)) in ordered.iter().enumerate() {
        stmt.execute(params![i as i64, id])?;
    }
    Ok(())
}

fn row_segment(r: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: r.get(0)?,
        top: r.get(1)?,
        bottom: r.get(2)?,
        present: r.get::<_, i64>(3)? != 0,
        gap_min_years: r.get(4)?,
        note: r.get(5)?,
        version: r.get(6)?,
    })
}

fn row_marker(r: &rusqlite::Row<'_>) -> rusqlite::Result<Marker> {
    let alt_json: String = r.get(8)?;
    Ok(Marker {
        id: r.get(0)?,
        depth: r.get(1)?,
        kind: r.get(2)?,
        hard: r.get::<_, i64>(3)? != 0,
        lo: r.get(4)?,
        hi: r.get(5)?,
        mean: r.get(6)?,
        sd: r.get(7)?,
        alt_means: serde_json::from_str(&alt_json).unwrap_or_default(),
        exchange_group: r.get(9)?,
        weight: r.get(10)?,
        excluded: r.get::<_, i64>(11)? != 0,
        note: r.get(12)?,
        version: r.get(13)?,
    })
}

fn row_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let snapshot_json: String = r.get(6)?;
    Ok(RunRecord {
        seq: r.get(0)?,
        kind: r.get(1)?,
        status: r.get(2)?,
        reason: r.get(3)?,
        seed: r.get::<_, i64>(4)? as u64,
        draws: r.get::<_, i64>(5)? as usize,
        snapshot: serde_json::from_str(&snapshot_json).expect("snapshot json"),
        result_json: r.get(7)?,
        conflict_json: r.get(8)?,
        parent_seq: r.get(9)?,
        rerun_json: r.get(10)?,
    })
}
