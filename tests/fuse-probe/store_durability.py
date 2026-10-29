#!/usr/bin/env python3
"""Focused SQLite/backing-file durability cost; no daemon, mount or account."""
import argparse
import sqlite3, tempfile, time, pathlib, statistics, shutil
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--work-dir', type=pathlib.Path, required=True)
base = parser.parse_args().work_dir
base.mkdir(parents=True, exist_ok=True)
results={k:[] for k in ('NORMAL','FULL')}
for mode in ('NORMAL','FULL','FULL','NORMAL','NORMAL','FULL'):
    root=pathlib.Path(tempfile.mkdtemp(prefix='bench-',dir=base))
    c=sqlite3.connect(root/'items.db'); c.execute('pragma journal_mode=WAL'); c.execute('pragma synchronous='+mode)
    c.execute('create table items(id integer primary key, changed integer, size integer)'); c.commit()
    t=time.monotonic()
    for n in range(10000):
        (root/str(n)).write_bytes(b'a'*64)
        c.execute('insert into items values(?,0,64)',(n,)); c.commit()
    created=time.monotonic()
    for n in range(1000):
        c.execute('update items set changed=1 where id=?',(n,)); c.commit()
        with (root/str(n)).open('r+b') as f: f.write(b'b'*64)
        c.execute('update items set changed=2 where id=?',(n,)); c.commit()
    rewritten=time.monotonic()
    for n in range(10000):
        c.execute('delete from items where id=?',(n,)); c.commit(); (root/str(n)).unlink()
    ended=time.monotonic(); results[mode].append(ended-t)
    print(mode,'create',round(created-t,3),'rewrite',round(rewritten-created,3),'delete',round(ended-rewritten,3),'total',round(ended-t,3),flush=True)
    c.close(); shutil.rmtree(root)
print('medians', {k:statistics.median(v) for k,v in results.items()},flush=True)
print('FULL overhead percent',100*(statistics.median(results['FULL'])/statistics.median(results['NORMAL'])-1),flush=True)
