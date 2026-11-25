"""Reference CME Globex session vectors from the IANA America/New_York zone,
and vectors for the localnet-only SCHEDULE_TEST_HOURLY (docs/localnet.md).

Regenerate after changing the schedule logic:
  python3 scripts/gen-session-vectors.py rust > programs/perps-core/src/session_vectors.rs
  python3 scripts/gen-session-vectors.py json > frontend/src/utils/sessionVectors.json
"""
import json, random, sys
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

NY = ZoneInfo("America/New_York")
UTC = timezone.utc

def local(ts): return datetime.fromtimestamp(ts, UTC).astimezone(NY)

def is_open(ts):
    d = local(ts); wd = (d.weekday() + 1) % 7  # 0=Sun
    secs = d.hour * 3600 + d.minute * 60 + d.second
    if wd == 6: return False
    if wd == 0: return secs >= 18 * 3600
    if wd == 5: return secs < 17 * 3600
    return not (17 * 3600 <= secs < 18 * 3600)

def weekly_close(ts):
    d = local(ts)
    day = d.date()
    while True:
        if day.weekday() == 4:
            close = datetime(day.year, day.month, day.day, 17, tzinfo=NY)
            c = int(close.timestamp())
            if c > ts: return c
        day += timedelta(days=1)

def offset(ts): return int(local(ts).utcoffset().total_seconds())

def boundaries():
    out = []
    for year in range(2025, 2029):
        for month in (3, 11):
            start = datetime(year, month, 1, tzinfo=NY) - timedelta(days=7)
            for i in range(28):
                day = (start + timedelta(days=i)).date()
                for hour in (17, 18):
                    t = int(datetime(day.year, day.month, day.day, hour, tzinfo=NY).timestamp())
                    out += [t - 1, t, t + 1]
    return out

random.seed(20200420)
lo = int(datetime(2024, 1, 1, tzinfo=UTC).timestamp()); hi = int(datetime(2040, 1, 1, tzinfo=UTC).timestamp())
session_ts = sorted(set(boundaries() + [random.randrange(lo, hi) for _ in range(400)]))
sessions = [(t, is_open(t), weekly_close(t) - t if is_open(t) else -1) for t in session_ts]

offset_ts = set()
for year in range(2024, 2041):
    for month in (3, 11):
        for day in range(1, 15):
            for h in (6, 7):
                t = int(datetime(year, month, day, h, tzinfo=UTC).timestamp())
                offset_ts |= {t - 1, t}
offsets = [(t, offset(t)) for t in sorted(offset_ts)]

# SCHEDULE_TEST_HOURLY: a one-hour "week" on Unix time, open [0, 3000) minus
# the breaks [750, 810), [1500, 1560), [2250, 2310). Written as explicit
# open segments, independently of the program's arithmetic. Its own RNG, so
# the CME vectors above are unchanged.
TEST_WEEK = 3600
TEST_SEGMENTS = [(0, 750), (810, 1500), (1560, 2250), (2310, 3000)]
TEST_WEEK_OPEN = sum(b - a for a, b in TEST_SEGMENTS)

def test_point(t):
    week, c = divmod(t, TEST_WEEK)  # Python divmod floors, like div_euclid
    seg = next((a for a, b in TEST_SEGMENTS if a <= c < b), None)
    open_until = week * TEST_WEEK_OPEN + sum(max(0, min(c, b) - a) for a, b in TEST_SEGMENTS)
    if seg is None:
        return (t, False, -1, -1, open_until)
    return (t, True, week * TEST_WEEK + 3000 - t, c - seg, open_until)

test_rng = random.Random(20261125)
test_ts = set()
for week in (-2, -1, 0, 1, 498_000, 498_001, 500_000):
    for edge in (0, 750, 810, 1500, 1560, 2250, 2310, 3000, 3599):
        t = week * TEST_WEEK + edge
        test_ts |= {t - 1, t, t + 1}
test_ts |= {test_rng.randrange(lo, hi) for _ in range(300)}
test_sessions = [test_point(t) for t in sorted(test_ts)]

fmt = sys.argv[1]
if fmt == "rust":
    print("// Generated from the IANA America/New_York zone by session_vectors.py; do not edit.")
    print("// (unix_ts, is_open, seconds_until_weekly_close or -1 when closed)")
    print("const SESSION_VECTORS: &[(i64, bool, i64)] = &[")
    for t, o, c in sessions: print(f"    ({t}, {str(o).lower()}, {c}),")
    print("];\n\n// (unix_ts, US Eastern UTC offset in seconds)")
    print("const OFFSET_VECTORS: &[(i64, i64)] = &[")
    for t, o in offsets: print(f"    ({t}, {o}),")
    print("];")
    print("\n// SCHEDULE_TEST_HOURLY (localnet only): (unix_ts, is_open,")
    print("// seconds_until_weekly_close or -1, seconds_since_open or -1, open seconds since Unix 0)")
    print("const TEST_SESSION_VECTORS: &[(i64, bool, i64, i64, i64)] = &[")
    for t, o, c, so, ou in test_sessions: print(f"    ({t}, {str(o).lower()}, {c}, {so}, {ou}),")
    print("];")
else:
    json.dump({"sessions": sessions, "offsets": offsets, "testSessions": test_sessions}, sys.stdout)
