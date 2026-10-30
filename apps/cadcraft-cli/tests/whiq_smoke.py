"""The platform's build smoke, against a built cadcraft-cli (the release workflow runs it).

    python3 apps/cadcraft-cli/tests/whiq_smoke.py path/to/cadcraft-cli

Like White Hat IQ's scripts/engines-fetch.mjs: --version, then `mcp` with both roots in a scratch
folder: initialize, tools/list, server_status (fonts.arabic must be named: a release carries
craft-fonts), then one render: a new drawing with an Arabic line in a style on that face, saved as
DWG and DXF, exported as PDF, previewed (the glyphs must be filled outlines of the face); the DWG
is opened back and the Arabic text read from it.
"""
import json, os, re, shutil, subprocess, sys, tempfile, zlib

ARABIC = "سطر عربي لاختبار الخط المضمّن"
bin_ = sys.argv[1]
v = subprocess.run([bin_, "--version"], capture_output=True, text=True, timeout=60)
assert v.returncode == 0, v.stderr
print("version:", v.stdout.strip())
d = tempfile.mkdtemp(prefix="cadcraft-smoke-")
os.makedirs(os.path.join(d, "in")); os.makedirs(os.path.join(d, "out"))
p = subprocess.Popen([bin_, "mcp", "--automation-read-root", os.path.join(d, "in"), "--automation-write-root", os.path.join(d, "out"), "--author", "smoke", "--busy-after-ms", "300", "--automation-no-old-copies"],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8")
n = 0
def rpc(method, params=None):
    global n
    n += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": n, "method": method, "params": params or {}}) + "\n"); p.stdin.flush()
    while True:
        line = p.stdout.readline()
        assert line, "the server exited"
        m = json.loads(line)
        if m.get("id") == n:
            return m
def call(tool, args):
    r = rpc("tools/call", {"name": tool, "arguments": args})["result"]
    assert not r.get("isError"), f"{tool}: {r['content'][0]['text']}"
    return r["structuredContent"]
init = rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "smoke", "version": "1"}})
print("server:", init["result"]["serverInfo"])
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"); p.stdin.flush()
tools = [t["name"] for t in rpc("tools/list")["result"]["tools"]]
print("tools:", ", ".join(tools))
st = call("server_status", {})
print("fonts:", st["fonts"])
assert st["fonts"]["arabic"], "no Arabic face embedded: a release must be built with craft-fonts"
doc = call("new_drawing", {"metric": True})
# A text style on the embedded Arabic face: its glyphs are written into the PDF as text, with the font.
call("edit", {"document": doc["document"], "ops": [
    {"op": "command", "command": "style", "params": {"name": "ARABIC", "font": st["fonts"]["arabic"]}},
    {"op": "command", "command": "style.current", "params": {"name": "ARABIC"}},
    {"op": "add", "type": "text", "at": [0, 0], "text": ARABIC, "height": 5},
    {"op": "add", "type": "line", "points": [[0, -2], [100, -2]]}]})
call("save_drawing", {"path": "smoke.dwg"})
call("export", {"path": "smoke.dxf"})
call("export", {"path": "smoke.pdf"})
call("render_preview", {})
dwg = open(os.path.join(d, "out", "smoke.dwg"), "rb").read(6)
assert dwg.startswith(b"AC10"), dwg
pdf = open(os.path.join(d, "out", "smoke.pdf"), "rb").read()
assert pdf.startswith(b"%PDF") and len(pdf) > 5000, len(pdf)
# CADCraft draws shaped Arabic runs into PDFs as filled outlines (they print; no font is embedded
# for them): the proof that the embedded face is used is that the glyphs are filled outlines
# (a stroke font or a missing face gives polylines or nothing).
pv = call("render_preview", {"window": [[-1, -1], [80, 9]]})
svg = pv["tiles"][0]["svg"]
assert svg.count("<path") >= 1, "the Arabic line drew no filled glyphs"
print("arabic glyph paths:", svg.count("<path"), "· pdf bytes:", len(pdf))
shutil.copy(os.path.join(d, "out", "smoke.dwg"), os.path.join(d, "in", "back.dwg"))
o = call("open_drawing", {"path": "back.dwg"})
t = call("read_drawing", {"document": o["document"], "section": "text"})
assert any(x["text"] == ARABIC for x in t["texts"]), t
p.stdin.close(); p.wait(timeout=60)
shutil.rmtree(d, ignore_errors=True)
print("smoke ok")
