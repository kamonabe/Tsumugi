#!/usr/bin/env python3
"""確認モードで「拾うもの」があるかを GitHub の REST API だけで機械的に判定する。

判定の正本は docs/loop-runbook.md の §3.1・§4.1・§5・§6.1・§6.4・§6.5・§7・§8.1・§14.4 である。
このスクリプトは手順書を置き換えない。手順書と食い違ったら、手順書に合わせてこのスクリプトを直す。

方針: 取りこぼし（拾うべきものを「無し」と言う）を避け、迷ったら「有り」と報告する。
「有り」と出たものを本当に処理するかは、エージェントが手順書の該当節を読んで決める。
「無し」のときだけ、エージェントは文書を読まずに終了してよい。

使い方:
    python3 pickup.py [--repo kamonabe1927/Tsumugi] [--now 2026-10-28T12:00:00Z]

出力: JSON（標準出力）。`nothing_to_do` が true なら、GitHub 側で拾うものは無い。
`slack_threads_to_read` が空でなければ、Slack のそのスレッドだけを読む（§6.1 の 2・7、§7.5 の確定後の返信）。
終了コード: 0 = 判定できた / それ以外 = 判定できなかった（このときは「有り」とみなして手順書どおりに進める）。
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone

# §3: kamonabe は user id で見分ける（login は改名で変わる。2026-10-31 に `kamonabe` → `kamonabe1927`）。
# 旧 login `kamonabe` は今は別人が取れるので、login では見分けない。
OWNER_ID = 19951033
OWNER = "kamonabe1927"  # 判定用の名前（OWNER_ID の書き込みはこの名前にそろえる）
LOOP_ACCOUNT = "kamonabe1927-stack"
LOOP_ACCOUNT_ID = 348768222
LOOP_LABELS = {"loop:design", "loop:impl"}
BLOCK_LABELS = {"loop:stuck", "loop:running"}
TRIAGE_EXCLUDE = {"needs-design", "loop:triaged", "wontfix"}
# §5: kamonabe の指示の書式（手順書 §10 によりエージェントはこれで書き始めない）
INSTRUCTION_PREFIXES = ("承認:", "保留:", "修正依頼:", "判断:", "[判断NG]")
REVIEW_HEAD = "🤖 Claude Code review（シニア）"  # §14.4
NO_FINDINGS = re.compile(r"^結論: 指摘なし\s*$", re.M)  # §14.4 の最後の行
WAIT_MARK = re.compile(r"判断待ち[:：]\s*Slack")  # §7.2 の 3
WAIT_RESOLVED = ("Slack で kamonabe が選択", "推奨案で確定")  # §7.3 の 3・§7.5 の 2
SLACK_LINK = re.compile(r"https://[\w.-]*slack\.com/archives/[^\s)）>]+")
SLACK_P = re.compile(r"/p(\d{10})(\d{6})")
SLACK_THREAD = re.compile(r"thread_ts=(\d{10})\.(\d{6})")
RUNNING_STALE = timedelta(hours=2)  # §4.1
WAIT_TIMEOUT = timedelta(hours=2)  # §7.5
AUTOMERGE_WAIT = timedelta(hours=3)  # §6.4 の 6
# §7.5 の「確定のあとで kamonabe が返信した場合（マージ済みなら needs-design issue）」を拾うため、
# 閉じてからこの時間までの PR・issue の判断待ちのスレッドも読む（手順書に期間の定めは無い）
CLOSED_WINDOW = timedelta(hours=72)


# ---------------------------------------------------------------- 判定（純粋関数）


def parse_time(s: str | None) -> datetime | None:
    if not s:
        return None
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def who(user: dict | None) -> str:
    """書き込みの投稿者を、判定に使う名前にする。kamonabe とループ用アカウントは user id で見分ける。"""
    user = user or {}
    if user.get("id") == OWNER_ID:
        return OWNER
    if user.get("id") == LOOP_ACCOUNT_ID:
        return LOOP_ACCOUNT
    # どちらでもない投稿者は、login が同じ文字列でも kamonabe・ループ用アカウントとして扱わない
    return "other:" + str(user.get("login", ""))


def is_agent(author: str, body: str | None) -> bool:
    """§3.1: エージェントの書き込みか（GitHub の条件）。"""
    if author == LOOP_ACCOUNT:
        return True
    return author == OWNER and (body or "").lstrip().startswith("🤖")


def is_owner(author: str, body: str | None) -> bool:
    """§3: 指示として扱う kamonabe の書き込みか。"""
    return author == OWNER and not is_agent(author, body)


def _w(author: str, body: str | None, created: str, updated: str | None, kind: str) -> dict:
    body = body or ""
    at = parse_time(created)
    # kamonabe が後から編集した書き込みは、編集した時刻に書かれたものとして扱う（取りこぼし防止）
    if is_owner(author, body) and updated:
        at = max(at, parse_time(updated))
    return {"author": author, "body": body, "at": at, "kind": kind}


def writes(item: dict) -> list[dict]:
    """PR・issue の本文と、付いた書き込み（コメント・レビュー・行コメント）を時刻順に並べる。"""
    out = []
    if item.get("user") and item.get("created_at"):
        out.append(_w(who(item["user"]), item.get("body"), item["created_at"], None, "body"))
    for c in item.get("comments", []):
        if c.get("user"):
            out.append(_w(who(c["user"]), c.get("body"), c["created_at"], c.get("updated_at"), "comment"))
    for r in item.get("reviews", []):
        if r.get("state") == "PENDING" or not r.get("submitted_at") or not r.get("user"):
            continue
        out.append(_w(who(r["user"]), r.get("body"), r["submitted_at"], None, "review")
                   | {"commit_id": r.get("commit_id")})
    for c in item.get("review_comments", []):
        if c.get("user"):
            out.append(_w(who(c["user"]), c.get("body"), c["created_at"], c.get("updated_at"),
                          "review_comment"))
    return sorted(out, key=lambda w: w["at"])


def owner_writes_after_last_agent(ws: list[dict]) -> list[dict]:
    """§6.1 の 1・3・4: ループが最後に書いた以降の kamonabe の書き込み。"""
    last_agent = max((w["at"] for w in ws if w["kind"] != "body" and is_agent(w["author"], w["body"])),
                     default=None)
    return [w for w in ws if w["kind"] != "body" and is_owner(w["author"], w["body"])
            and (last_agent is None or w["at"] > last_agent)]


def slack_ts(text: str) -> set[str]:
    """本文に現れる Slack の ts（`1793144318.433259` の形）をすべて取り出す。"""
    return {f"{a}.{b}" for a, b in SLACK_P.findall(text)} | {f"{a}.{b}" for a, b in SLACK_THREAD.findall(text)}


def judgement_waits(ws: list[dict]) -> list[dict]:
    """§7.2 の 3 で残した「判断待ち: Slack <リンク>」と、その後の解決の有無。

    解決 = 待ちのあとに §7.3 の 3 か §7.5 の 2 の転記（エージェントの書き込み）があり、
    その転記が同じスレッドの ts を含む（ts を含まない転記は、それより前の待ちをすべて解決したものとみなす）。
    """
    agent_ts: set[str] = set()
    for w in ws:
        if is_agent(w["author"], w["body"]):
            agent_ts |= slack_ts(w["body"])
    waits = []
    for i, w in enumerate(ws):
        if not (is_agent(w["author"], w["body"]) and WAIT_MARK.search(w["body"])):
            continue
        links = SLACK_LINK.findall(w["body"])
        thread = {f"{a}.{b}" for link in links for a, b in SLACK_P.findall(link)}
        resolved = False
        for x in ws[i + 1:]:
            if not (is_agent(x["author"], x["body"]) and any(m in x["body"] for m in WAIT_RESOLVED)):
                continue
            mentioned = slack_ts(x["body"])
            if not mentioned or (thread and thread & mentioned):
                resolved = True
                break
        posted = w["at"]
        for ts in thread:  # Slack の親投稿の時刻のほうが早ければそちらを使う
            posted = min(posted, datetime.fromtimestamp(float(ts), tz=timezone.utc))
        waits.append({"posted_at": posted, "links": links, "resolved": resolved,
                      "referenced_ts": sorted(agent_ts)})
    return waits


def has_no_findings_review_after(ws: list[dict], head_at: datetime | None, head_sha: str | None) -> bool:
    """§6.4 の 4・§6.5 の 3: 最新 commit より後に「指摘なし」のレビュー役のレビューがあるか。

    時刻に加えて、レビューが最新 commit を対象にしていること（commit_id か、見出しの commit の先頭 7 桁）も求める。
    厳しく判定するほど「再レビューの候補」として拾われるので、取りこぼしにはならない。
    """
    for w in ws:
        if head_at is None or w["at"] <= head_at:
            continue
        if not (is_agent(w["author"], w["body"]) and REVIEW_HEAD in w["body"] and NO_FINDINGS.search(w["body"])):
            continue
        if head_sha and not (w.get("commit_id") == head_sha or head_sha[:7] in w["body"]):
            continue
        return True
    return False


def has_hold(ws: list[dict]) -> bool:
    """§5・§6.4 の 7: kamonabe の `保留:` があるか。"""
    return any(is_owner(w["author"], w["body"]) and w["body"].lstrip().startswith("保留:") for w in ws)


def labels_of(item: dict) -> set[str]:
    return {lb["name"] for lb in item.get("labels", []) if lb}


def slack_entry(where: str, wait: dict, now: datetime) -> dict:
    return {"where": where, "links": wait["links"], "resolved": wait["resolved"],
            "over_2h": (not wait["resolved"]) and now - wait["posted_at"] >= WAIT_TIMEOUT,
            "referenced_ts": wait["referenced_ts"]}


def judge(prs: list[dict], issues: list[dict], now: datetime, closed: list[dict] = ()) -> dict:
    """GitHub から集めたデータを §6.1 などに照らして判定する。

    prs: 開いている PR（`comments`・`reviews`・`review_comments`・`head_committed_at`・
         `running_labeled_at` を付け足したもの）
    issues: 開いている issue（PR を除く。`comments` を付け足したもの）
    closed: 閉じてから CLOSED_WINDOW 以内の PR・issue（`comments` を付け足したもの）
    """
    found: dict[str, list] = {
        "owner_comments_on_loop_prs": [],   # §6.1 の 1
        "owner_instructions_after_head": [],  # §5 の書式で、最新 commit より後の kamonabe の指示
        "owner_comments_on_issues": [],     # §6.1 の 3・4（ほかの issue の返信も念のため拾う）
        "issues_to_triage": [],             # §6.1 の 5・§8.1
        "rereview_candidates": [],          # §6.1 の 6・§6.5
        "automerge_candidates": [],         # §6.4（残りの条件は手順書で確かめる）
        "stale_running": [],                # §4.1
        "waits_without_link": [],           # 判断待ちの印はあるがスレッドが特定できない
    }
    slack: list[dict] = []  # §6.1 の 2・7

    def add_waits(where: str, waits: list[dict]) -> None:
        for w in waits:
            if not w["links"]:
                found["waits_without_link"].append({"where": where, "resolved": w["resolved"]})
            else:
                # 未解決の待ちは返信と 2 時間の確定を見る。確定済みでも、§7.5 の
                # 「確定のあとで kamonabe が返信した場合」を拾うためにスレッドを読む。
                slack.append(slack_entry(where, w, now))

    for pr in prs:
        lbs = labels_of(pr)
        if not lbs & LOOP_LABELS:
            continue
        n = pr["number"]
        ws = writes(pr)
        head_at = parse_time(pr.get("head_committed_at"))
        head_sha = (pr.get("head") or {}).get("sha")
        waits = judgement_waits(ws)
        add_waits(f"PR #{n}", waits)

        if "loop:running" in lbs:
            at = parse_time(pr.get("running_labeled_at"))
            if at is None or now - at >= RUNNING_STALE:
                found["stale_running"].append({"pr": n, "labeled_at": pr.get("running_labeled_at")})
            # §4.1: 目印の付いた PR は次の回に回す（残った目印だけは拾う）
            continue

        owner_ws = owner_writes_after_last_agent(ws)
        if owner_ws:
            found["owner_comments_on_loop_prs"].append(
                {"pr": n, "count": len(owner_ws), "first": owner_ws[0]["body"][:80]})
        # 目印の付いていた間に書かれた指示は、そのあとのエージェントの書き込みで上の判定から漏れる。
        # 最新 commit より後の §5 の書式の指示は、処理済みかどうかを手順書で確かめるために拾う。
        instr = [w for w in ws if w["kind"] != "body" and is_owner(w["author"], w["body"])
                 and (head_at is None or w["at"] > head_at)
                 and w["body"].lstrip().startswith(INSTRUCTION_PREFIXES)
                 and not w["body"].lstrip().startswith("保留:")]
        if instr:
            found["owner_instructions_after_head"].append({"pr": n, "first": instr[0]["body"][:80]})

        if lbs & BLOCK_LABELS:
            continue
        open_waits = [w for w in waits if not w["resolved"]]
        reviewed = has_no_findings_review_after(ws, head_at, head_sha)
        if not open_waits and not reviewed:
            found["rereview_candidates"].append({"pr": n})
        if (reviewed and not open_waits and not has_hold(ws) and head_at is not None
                and now - head_at >= AUTOMERGE_WAIT
                and not any(is_owner(w["author"], w["body"]) and w["at"] > head_at for w in ws)):
            found["automerge_candidates"].append({"pr": n})

    for iss in issues:
        lbs = labels_of(iss)
        n = iss["number"]
        ws = writes(iss)
        owner_ws = owner_writes_after_last_agent(ws)
        author = who(iss.get("user"))
        new_for_triage = author == OWNER and not is_agent(author, iss.get("body")) and not lbs & TRIAGE_EXCLUDE
        if new_for_triage:
            found["issues_to_triage"].append({"issue": n, "title": (iss.get("title") or "")[:80]})
        elif owner_ws:
            found["owner_comments_on_issues"].append(
                {"issue": n, "labels": sorted(lbs), "count": len(owner_ws)})
        add_waits(f"issue #{n}", judgement_waits(ws))

    for item in closed:
        kind = "PR" if "pull_request" in item else "issue"
        add_waits(f"{kind} #{item['number']}（閉じている）", judgement_waits(writes(item)))

    nothing = not any(found.values()) and not slack
    return {"nothing_to_do": nothing, "found": found, "slack_threads_to_read": slack}


# ---------------------------------------------------------------- GitHub からの取得


def gh(path: str) -> list | dict:
    res = subprocess.run(["gh", "api", "--paginate", "--slurp", path],
                         capture_output=True, text=True, check=True)
    pages = json.loads(res.stdout)
    if pages and isinstance(pages[0], list):
        return [x for page in pages for x in page]
    return pages[0] if len(pages) == 1 else pages


def collect(repo: str, now: datetime) -> tuple[list[dict], list[dict], list[dict]]:
    prs = gh(f"repos/{repo}/pulls?state=open&per_page=100")
    for pr in prs:
        n = pr["number"]
        if not labels_of(pr) & LOOP_LABELS:
            continue
        pr["comments"] = gh(f"repos/{repo}/issues/{n}/comments?per_page=100")
        pr["reviews"] = gh(f"repos/{repo}/pulls/{n}/reviews?per_page=100")
        pr["review_comments"] = gh(f"repos/{repo}/pulls/{n}/comments?per_page=100")
        commits = gh(f"repos/{repo}/pulls/{n}/commits?per_page=100")
        if commits:
            pr["head_committed_at"] = commits[-1]["commit"]["committer"]["date"]
        if "loop:running" in labels_of(pr):
            events = gh(f"repos/{repo}/issues/{n}/events?per_page=100")
            labeled = [e["created_at"] for e in events
                       if e.get("event") == "labeled" and (e.get("label") or {}).get("name") == "loop:running"]
            pr["running_labeled_at"] = max(labeled) if labeled else None
    issues = [i for i in gh(f"repos/{repo}/issues?state=open&per_page=100") if "pull_request" not in i]
    for iss in issues:
        iss["comments"] = gh(f"repos/{repo}/issues/{iss['number']}/comments?per_page=100") \
            if iss.get("comments") else []
    since = (now - CLOSED_WINDOW).strftime("%Y-%m-%dT%H:%M:%SZ")
    closed = [i for i in gh(f"repos/{repo}/issues?state=closed&since={since}&per_page=100")
              if parse_time(i.get("closed_at")) and now - parse_time(i["closed_at"]) <= CLOSED_WINDOW]
    for item in closed:
        item["comments"] = gh(f"repos/{repo}/issues/{item['number']}/comments?per_page=100") \
            if item.get("comments") else []
    return prs, issues, closed


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default="kamonabe1927/Tsumugi")
    ap.add_argument("--now", help="判定に使う現在時刻（ISO 8601。試験用）")
    args = ap.parse_args()
    now = parse_time(args.now) if args.now else datetime.now(timezone.utc)
    try:
        prs, issues, closed = collect(args.repo, now)
        result = judge(prs, issues, now, closed)
    except Exception as e:  # noqa: BLE001 判定できなければ「有り」として扱わせる
        detail = getattr(e, "stderr", "") or repr(e)
        print(json.dumps({"nothing_to_do": False, "error": str(detail).strip()[:500]}, ensure_ascii=False))
        return 2
    result["checked_at"] = now.isoformat()
    print(json.dumps(result, ensure_ascii=False, indent=1, default=str))
    return 0


if __name__ == "__main__":
    sys.exit(main())
