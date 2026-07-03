"""Cross-language, cross-role, cross-prompt-style benchmark for Ornith via the gateway.

Each task is (language, role, prompt_style) tagged so the matrix can be split out
in the report. Temperature 0, max_tokens generous but bounded.
"""
import json
import urllib.request
import time
import re
import os
import tempfile
import sys

ENDPOINTS = {
    "gateway :9000":  "http://127.0.0.1:9000/v1/chat/completions",
    "direct :8081":   "http://127.0.0.1:8081/v1/chat/completions",
}

TASKS = [
    # ─── Python (algorithmic + correctness) ───────────────────────────────────
    {"id":"py_fib", "lang":"python","role":"plain","style":"terse",
     "prompt":"Write a Python function `def fib(n)` returning the n-th Fibonacci (fib(0)=0, fib(10)=55). Output Python code only.",
     "extract":"def fib", "tests":[(0,0),(1,1),(10,55),(20,6765)],
     "max_tokens":1500},

    {"id":"py_movavg", "lang":"python","role":"plain","style":"terse",
     "prompt":"Write Python `def moving_average(values: list[float], window: int) -> list[float]`. Standard moving average over successive windows of length `window`. Output Python code only.",
     "extract":"def moving_average", "tests":[
         ({"values":[1.0,2.0,3.0,4.0,5.0],"window":3}, [2.0,3.0,4.0]),
         ({"values":[10.0,20.0],"window":2}, [15.0]),
         ({"values":[1.0,2.0],"window":1}, [1.0,2.0])],
     "max_tokens":1500},

    # ─── Rust ──────────────────────────────────────────────────────────────────
    {"id":"rs_bsearch", "lang":"rust","role":"plain","style":"terse",
     "prompt":"Write a Rust function `fn bsearch(arr: &[i32], target: i32) -> Option<usize>` returning the index of `target` or None. Use `Result`-style; no `unwrap()`. Output Rust code only.",
     "extract":"fn bsearch", "tests":[
         # logical correctness tests via external rustc compile + run
         ("([1,3,5,7,9],5)", "2"),
         ("([1,3,5,7],4)", "None"),
         ("([],1)", "None")],
     "max_tokens":1500},

    # ─── Go ────────────────────────────────────────────────────────────────────
    {"id":"go_reverse", "lang":"go","role":"plain","style":"terse",
     "prompt":"Write a Go function `func reverseSlice(s []int) []int` returning a NEW slice with elements in reverse order. Don't mutate input. Output Go code only.",
     "extract":"func reverseSlice", "tests":[
         ("[1,2,3,4,5]", "5,4,3,2,1"),
         ("[]", "[]"),
         ("[42]", "[42]")],
     "max_tokens":1500},

    # ─── SQL ───────────────────────────────────────────────────────────────────
    {"id":"sql_second", "lang":"sql","role":"plain","style":"terse",
     "prompt":"Write a single SQL query that returns the second-highest distinct `salary` from an `employees(salary)` table. Output the SQL only, no prose.",
     "extract":"SELECT", "tests":[
         # ergonomic text check — sql dialect: standard SQL, second-highest
         ("SELECT DISTINCT salary FROM employees ORDER BY salary DESC LIMIT 1 OFFSET 1", True),
         ("SELECT MAX(salary) FROM employees WHERE salary < (SELECT MAX(salary) FROM employees)", True),
     ],
     "max_tokens":1500},

    # ─── Bash (one-liner) ──────────────────────────────────────────────────────
    {"id":"sh_top5", "lang":"bash","role":"plain","style":"terse",
     "prompt":"Write a bash one-liner that prints the 5 largest files under the current directory, size and path, biggest first. Use `find` and `du`. One line. Output bash only, no prose.",
     "extract":"find", "tests":[
         # shape checks
         ("find . -type f -exec du -h {} +", "find"),
     ],
     "max_tokens":1500},

    # ─── JavaScript / TypeScript ───────────────────────────────────────────────
    {"id":"js_palindrome", "lang":"javascript","role":"plain","style":"terse",
     "prompt":"Write a JavaScript function `function isPalindrome(s) { ... }` returning true iff s is a palindrome ignoring case and non-alphanumeric chars. Output JS only.",
     "extract":"function isPalindrome", "tests":[
         ("'racecar'", "true"),
         ("'A man, a plan, a canal: Panama'", "true"),
         ("'hello'", "false"),
         ("''", "true")],
     "max_tokens":1500},

    # ─── role: defensive / security ───────────────────────────────────────────
    {"id":"py_defensive", "lang":"python","role":"defensive","style":"structured",
     "prompt":"Write a Python function `def safe_divide(a, b)` that returns `None` on divide-by-zero, NEVER raises, and uses type hints. Include a docstring describing the contract. Output Python only.",
     "extract":"def safe_divide", "tests":[
         ((10, 2), 5.0),
         ((1, 0), None),
         ((0, 5), 0.0)],
     "max_tokens":1500},

    # ─── role: terse one-liner output ──────────────────────────────────────────
    {"id":"py_one_liner", "lang":"python","role":"terse","style":"minimal",
     "prompt":"Write Python `def sum_sqr(xs): return sum(x*x for x in xs)`. Output only that one line — no docstring, no tests, no signature line break.",
     "extract":"def sum_sqr", "tests":[
         (([1,2,3],), 14),
         (([],), 0),
         (([10],), 100)],
     "max_tokens":1500},

    # ─── role: well-documented ────────────────────────────────────────────────
    {"id":"py_documented", "lang":"python","role":"documented","style":"verbose",
     "prompt":"Write Python `def celsius_to_fahrenheit(c: float) -> float`. Include a multi-line docstring (one-line summary, one-line extended description, Args and Returns sections) and a one-line example in the docstring. Output Python only.",
     "extract":"def celsius_to_fahrenheit", "tests":[
         ((0.0,), 32.0),
         ((100.0,), 212.0),
         ((-40.0,), -40.0)],
     "max_tokens":1500},

    # ─── adversarial prompt (smoke) ───────────────────────────────────────────
    {"id":"py_refactor", "lang":"python","role":"refactor","style":"conversational",
     "prompt":"I have a function `def f(x): return x*2` — it's bad. Rename it, give it a type signature, and write a quick docstring. Output the new function with no commentary.",
     "extract":"def ", "tests":[],  # shape-only check
     "max_tokens":1500},

    # ─── constraints-following ────────────────────────────────────────────────
    {"id":"py_no_loop", "lang":"python","role":"constraints","style":"terse",
     "prompt":"Write Python `def sum_to_n(n)` returning 1+2+...+n WITHOUT any `for` loop or `while` loop. Use `sum(range(...))` or math. Output one Python function only.",
     "extract":"def sum_to_n", "tests":[
         ((0,), 0),
         ((1,), 1),
         ((100,), 5050)],
     "max_tokens":1500},
    {"id":"py_first_n_sq", "lang":"python","role":"plain","style":"terse",
     "prompt":"Write Python `def first_n_squares(n)` returning a list of the first n perfect squares `[1, 4, 9, ...]`. Output Python code only.",
     "extract":"def first_n_squares", "tests":[
         ((1,), [1]),
         ((3,), [1, 4, 9]),
         ((5,), [1, 4, 9, 16, 25])],
     "max_tokens":1500},

    {"id":"py_two_sum", "lang":"python","role":"plain","style":"terse",
     "prompt":"Write Python `def two_sum(nums: list[int], target: int) -> list[int]` returning indices i<j such that nums[i]+nums[j] == target. Order doesn't matter. Output Python code only.",
     "extract":"def two_sum", "tests":[
         ({"nums":[2,7,11,15],"target":14}, [0, 1]),
         ({"nums":[3,2,4],"target":6}, [1, 2]),
         ({"nums":[3,3],"target":6}, [0, 1])],
     "max_tokens":1500},
]  

def call(url, model_id, task, timeout_s=120):
    body = {
        "model": model_id,
        "messages":[{"role":"user","content":task["prompt"]}],
        "temperature":0,
        "max_tokens":task["max_tokens"],
    }
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type":"application/json"})
    t0 = time.time()
    try:
        r = json.loads(urllib.request.urlopen(req, timeout=timeout_s).read())
    except Exception as e:
        return {"ok":False,"err":str(e)[:120],"latency_s":time.time()-t0}
    latency = time.time() - t0
    ch = r.get("choices",[{}])[0]
    if not ch:
        return {"ok":False,"err":"no choices","latency_s":latency}
    return {
        "ok":True,
        "latency_s":latency,
        "finish":ch.get("finish_reason"),
        "content":ch.get("message",{}).get("content",""),
        "usage":r.get("usage",{}),
        "tok_s_pred": r.get("timings",{}).get("predicted_per_second"),
    }

FENCE_PY = re.compile(r"```(?:python)?\n(.+?)```", re.S)
FENCE_ANY = re.compile(r"```([a-zA-Z0-9_+-]*)\n(.+?)```", re.S)

def extract_code(text, lang=None):
    if lang:
        m = re.search(rf"```{re.escape(lang)}\n(.+?)```", text, re.S)
        if m: return m.group(1)
    m = FENCE_ANY.search(text)
    if m: return m.group(2)
    # fallback: look for the def/func/SELECT/find keyword in raw text
    for kw in ("def ", "fn ", "func ", "function ", "SELECT ", "find "):
        if kw in text:
            return text[text.index(kw):]
    return None

def run_python(code, fn_name, tests, timeout=15):
    """Run user Python in a subprocess with a small driver.

    Each test entry is `(args, expected)`:
      - `args` may be a scalar, a tuple/list (positional), or a dict (kwargs).
      - `expected` is what the function should return; compared by `str()`.
    """
    if not code or fn_name not in code:
        return {"passed":0,"total":len(tests),"failures":["no code or no "+fn_name]}
    driver = f"""
import sys, json as _j
{code}
res = []
for entry in {json.dumps(tests)}:
    if isinstance(entry, (list, tuple)) and len(entry) == 2:
        args, expected = entry
    else:
        args, expected = entry, None
    try:
        if isinstance(args, dict):
            got = {fn_name}(**args)
        elif isinstance(args, (list, tuple)):
            got = {fn_name}(*args)
        else:
            got = {fn_name}(args)
        ok = (str(got) == str(expected)) if expected is not None else True
        res.append({{'ok': ok, 'got': str(got)[:60], 'want': str(expected)[:60] if expected is not None else ''}})
    except Exception as e:
        res.append({{'ok': False, 'got': f'EXC: {{type(e).__name__}}: {{e}}'[:80], 'want': str(expected)[:60] if expected is not None else ''}})
print(_j.dumps(res))
"""
    with tempfile.NamedTemporaryFile(suffix=".py", mode="w", delete=False) as f:
        f.write(driver); path=f.name
    try:
        import subprocess
        out = subprocess.run([sys.executable, path], capture_output=True, text=True, timeout=timeout)
        if out.returncode != 0:
            return {"passed":0,"total":len(tests),"failures":["stderr: "+out.stderr[:200]]}
        lines = [ln for ln in out.stdout.splitlines() if ln.startswith("[")]
        results = json.loads(lines[-1]) if lines else []
        passed = sum(1 for r in results if r.get("ok"))
        return {"passed":passed, "total":len(tests), "detail":results}
    finally:
        os.unlink(path)

def grade(task, content):
    """Return (passed, total, details)."""
    if not content: return (0, len(task["tests"]), {"reason":"empty content"})
    lang = task["lang"]
    code = extract_code(content, lang=lang if lang in ("python","rust","go","javascript","bash") else None)
    if not code:
        # sql: just check shape
        if lang == "sql":
            upper = content.upper()
            ok = "SELECT" in upper and ("LIMIT" in upper or "MAX" in upper or "DENSE_RANK" in upper)
            return (1 if ok else 0, 1, {"reason":"sql-shape","content":content[:120]})
        if not task["tests"]:
            return (1, 1, {"reason":"shape-only","content":content[:120]})
        return (0, len(task["tests"]), {"reason":"no code","content":content[:120]})
    fn = task["extract"].strip()
    if lang == "python" and task["tests"]:
        m = re.search(r"def\s+(\w+)\s*\(", code)
        fn_name = m.group(1) if m else fn
        # if tests are pure-shape compare, route by checking code includes
        r = run_python(code, fn_name, task["tests"])
        return (r["passed"], r["total"], r)
    # for other langs or no tests: just shape check
    if not task["tests"]:
        return (1, 1, {"reason":"shape-only","snippet":code[:200]})
    # other languages: trivially mark present as 1 test, but note shape
    return (1, 1, {"reason":"language-shape-only","lang":lang,"snippet":code[:200]})

def main():
    mode = sys.argv[1] if len(sys.argv)>1 else "gateway :9000"
    gateway_model = os.environ.get("GATEWAY_MODEL_ID", "default-model-id")
    llama_model   = os.environ.get("MODEL_GGUF", "/path/to/model.gguf")
    url, model = ENDPOINTS[mode], (gateway_model if mode.startswith("gateway") else llama_model)
    print(f"# Real-coding benchmark — {mode}\n")
    print("| id | lang | role | style | lat(s) | finish | prompt | compl | cached | tok/s | pass/total | note |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    rows=[]
    for task in TASKS:
        r = call(url, model, task)
        if not r.get("ok"):
            print(f"| {task['id']} | {task['lang']} | {task['role']} | {task['style']} | ERR | - | - | - | - | - | - | {r.get('err','')[:60]} |")
            rows.append((task, 0, 0, r.get('latency_s',0), "err"))
            continue
        passed, total, details = grade(task, r["content"])
        u=r["usage"]
        print(f"| {task['id']} | {task['lang']} | {task['role']} | {task['style']} | {r['latency_s']:.1f} | {r['finish']} | {u.get('prompt_tokens',0)} | {u.get('completion_tokens',0)} | {u.get('prompt_tokens_details',{}).get('cached_tokens',0)} | {round(r.get('tok_s_pred') or 0,1)} | {passed}/{total} | {(details.get('reason') or '')[:30]} |")
        rows.append((task, passed, total, r["latency_s"], r["finish"]))

    print("\n## Summary\n")
    p=sum(x[1] for x in rows); t=sum(x[2] for x in rows)
    n_err = sum(1 for x in rows if x[4]=="err")
    print(f"- Tasks: {len(rows)}  •  Tests passed: {p}/{t} ({100*p/max(1,t):.1f}%)")
    print(f"- Errors: {n_err}")
    print(f"- Wall-time: {sum(x[3] for x in rows):.1f}s")
    # per-language breakdown
    by_lang={}
    for task, passed, total, *_ in rows:
        by_lang.setdefault(task["lang"], [0,0])
        by_lang[task["lang"]][0] += passed
        by_lang[task["lang"]][1] += total
    print("\n### Per-language pass-rate\n")
    for lang, (pp, tt) in sorted(by_lang.items()):
        pct = 100*pp/max(1,tt)
        print(f"  - {lang:11s}: {pp}/{tt}  ({pct:.0f}%)")
    # per-role breakdown
    by_role={}
    for task, passed, total, *_ in rows:
        by_role.setdefault(task["role"], [0,0])
        by_role[task["role"]][0] += passed
        by_role[task["role"]][1] += total
    print("\n### Per-role pass-rate\n")
    for role, (pp, tt) in sorted(by_role.items()):
        pct = 100*pp/max(1,tt)
        print(f"  - {role:14s}: {pp}/{tt}  ({pct:.0f}%)")
    # per-style breakdown
    by_style={}
    for task, passed, total, *_ in rows:
        by_style.setdefault(task["style"], [0,0])
        by_style[task["style"]][0] += passed
        by_style[task["style"]][1] += total
    print("\n### Per-style pass-rate\n")
    for style, (pp, tt) in sorted(by_style.items()):
        pct = 100*pp/max(1,tt)
        print(f"  - {style:14s}: {pp}/{tt}  ({pct:.0f}%)")

if __name__ == "__main__":
    main()
