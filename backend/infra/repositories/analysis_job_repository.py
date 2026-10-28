"""Durable analysis queue, kept beside the library in a small SQLite database."""
import json
import os
import sqlite3
import time
import uuid
from contextlib import contextmanager, closing
import infra.database.connection as connection


class AnalysisJobRepository:
    def __init__(self, path):
        self.path = path

    @contextmanager
    def connect(self):
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        with connection.database_activity, closing(sqlite3.connect(self.path, timeout=30)) as con, con:
            con.row_factory = sqlite3.Row
            con.execute("PRAGMA journal_mode=WAL")
            con.execute("CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, status TEXT, config TEXT, created REAL, updated REAL, elapsed REAL DEFAULT 0, error TEXT)")
            con.execute("CREATE TABLE IF NOT EXISTS items (job_id TEXT, track_id INTEGER, filepath TEXT, status TEXT DEFAULT 'pending', seconds REAL DEFAULT 0, error TEXT, PRIMARY KEY(job_id, track_id))")
            yield con

    def recover(self):
        with self.connect() as con:
            # Automatic embedding jobs resume by themselves; user jobs wait
            # for an explicit resume, including ones queued behind the
            # background job before the restart.
            for row in con.execute("SELECT id,config FROM jobs WHERE status IN ('running','pausing','queued')").fetchall():
                status = "queued" if json.loads(row["config"]).get("background") else "paused"
                con.execute("UPDATE jobs SET status=? WHERE id=?", (status, row["id"]))
            con.execute("UPDATE items SET status='pending' WHERE status='running'")

    def create(self, config, tracks, status="paused"):
        job_id = uuid.uuid4().hex
        now = time.time()
        with self.connect() as con:
            con.execute("INSERT INTO jobs (id,status,config,created,updated) VALUES (?,?,?,?,?)",
                        (job_id, status, json.dumps(config), now, now))
            con.executemany("INSERT INTO items (job_id,track_id,filepath) VALUES (?,?,?)",
                            [(job_id, t["id"], t["filepath"]) for t in tracks])
        return job_id

    def _ids(self, status=None):
        # Looking for work must not create the queue (restore relies on that).
        if not os.path.exists(self.path):
            return []
        with self.connect() as con:
            where = "WHERE status=? " if status else ""
            rows = con.execute(f"SELECT id,config FROM jobs {where}ORDER BY created", (status,) if status else ()).fetchall()
        return [(row["id"], bool(json.loads(row["config"]).get("background"))) for row in rows]

    def next_queued(self, background=None):
        """Oldest queued job; user jobs before automatic background ones."""
        rows = [(job_id, bg) for job_id, bg in self._ids("queued") if background is None or bg == background]
        rows.sort(key=lambda row: row[1])
        return rows[0][0] if rows else None

    def latest_id(self, background=None):
        rows = [job_id for job_id, bg in self._ids() if background is None or bg == background]
        return rows[-1] if rows else None

    def get(self, job_id=None):
        with self.connect() as con:
            row = con.execute("SELECT * FROM jobs WHERE id=?", (job_id,)).fetchone() if job_id else con.execute("SELECT * FROM jobs ORDER BY created DESC LIMIT 1").fetchone()
            if row is None:
                if job_id:
                    raise ValueError("Analysis job not found")
                return {"status": "idle"}
            result = dict(row)
            result["config"] = json.loads(result["config"])
            counts = dict(con.execute("SELECT status,count(*) FROM items WHERE job_id=? GROUP BY status", (row["id"],)).fetchall())
            result["counts"] = {s: counts.get(s, 0) for s in ("pending", "running", "completed", "skipped", "failed")}
            result["total"] = sum(counts.values())
            result["errors"] = [dict(r) for r in con.execute("SELECT track_id,filepath,error FROM items WHERE job_id=? AND status='failed' LIMIT 20", (row["id"],))]
            result["current_tracks"] = [dict(r) for r in con.execute("SELECT track_id,filepath FROM items WHERE job_id=? AND status='running'", (row["id"],))]
            done = counts.get("completed", 0) + counts.get("skipped", 0) + counts.get("failed", 0)
            result["remaining"] = result["total"] - done
            result["estimated_remaining_seconds"] = round(result["elapsed"] / done * result["remaining"], 1) if done else None
            return result

    def update_job(self, job_id, status, elapsed=None, error=None):
        with self.connect() as con:
            con.execute("UPDATE jobs SET status=?,updated=?,elapsed=COALESCE(?,elapsed),error=? WHERE id=?",
                        (status, time.time(), elapsed, error, job_id))

    def prepare_resume(self, job_id, workers, retry_failed):
        job = self.get(job_id)
        config = job["config"]
        config["workers"] = workers
        with self.connect() as con:
            con.execute("UPDATE jobs SET config=? WHERE id=?", (json.dumps(config), job_id))
            con.execute("UPDATE items SET status='pending' WHERE job_id=? AND status='running'", (job_id,))
            if retry_failed:
                con.execute("UPDATE items SET status='pending',error=NULL WHERE job_id=? AND status='failed'", (job_id,))

    def claim(self, job_id):
        with self.connect() as con:
            row = con.execute("SELECT track_id,filepath FROM items WHERE job_id=? AND status='pending' ORDER BY rowid LIMIT 1", (job_id,)).fetchone()
            if row:
                con.execute("UPDATE items SET status='running' WHERE job_id=? AND track_id=?", (job_id, row["track_id"]))
                return dict(row)
        return None

    def finish(self, job_id, track_id, status, seconds=0, error=None):
        with self.connect() as con:
            con.execute("UPDATE items SET status=?,seconds=?,error=? WHERE job_id=? AND track_id=?",
                        (status, seconds, error, job_id, track_id))
