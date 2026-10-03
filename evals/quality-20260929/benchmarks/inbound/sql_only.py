import sqlite3
import time
import statistics

conn = sqlite3.connect(':memory:')
conn.executescript('CREATE TABLE refs(id INTEGER PRIMARY KEY,target_qualified TEXT,target_symbol_hint INTEGER,target_name TEXT,confidence TEXT,kind TEXT); CREATE INDEX refs_target_qualified ON refs(target_qualified) WHERE target_qualified IS NOT NULL; CREATE INDEX refs_target_name ON refs(target_name); CREATE TABLE symbols(id INTEGER PRIMARY KEY,qualified TEXT); INSERT INTO symbols VALUES(1,"crate::Target");')
conn.executemany('INSERT INTO refs VALUES(?,?,?,?,?,?)', ((i, 'crate::Target' if i == 123456 else f'crate::other{i % 100}', 1 if i == 123456 else i + 2, 'Target' if i == 123456 else 'other', 'exact', 'call') for i in range(300000)))
predicates = {
    'before': 'target_symbol_hint = ?1 OR (target_symbol_hint IS NULL AND target_qualified = ?2)',
    'after': 'target_qualified = ?2 AND (target_symbol_hint = ?1 OR NOT EXISTS (SELECT 1 FROM symbols hinted WHERE hinted.id = refs.target_symbol_hint AND hinted.qualified = refs.target_qualified))',
}
print('sqlite_version=', sqlite3.sqlite_version)
for name, predicate in predicates.items():
    sql = 'SELECT count(*) FROM refs WHERE ' + predicate
    plan = conn.execute('EXPLAIN QUERY PLAN ' + sql, (1, 'crate::Target')).fetchall()
    print(name, 'plan:', plan)
    if name == 'after':
        assert any('refs_target_qualified' in row[3] for row in plan), plan
        assert not any(row[3] == 'SCAN refs' for row in plan), plan
    elapsed = []
    for _ in range(20):
        start = time.perf_counter()
        count = conn.execute(sql, (1, 'crate::Target')).fetchone()[0]
        elapsed.append((time.perf_counter() - start) * 1000)
        assert count == 1
    print(name, 'count=', count, 'median_ms=', round(statistics.median(elapsed), 3))
