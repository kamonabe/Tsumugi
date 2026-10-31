"""pickup.py の判定（judge）の試験。GitHub には接続しない。

実行: python3 -m unittest discover -s .claude/skills/loop-check/scripts
"""

import unittest
from datetime import datetime, timezone

from pickup import judge

NOW = datetime(2026, 10, 28, 12, 0, tzinfo=timezone.utc)
STACK = "kamonabe1927-stack"
OWNER = "kamonabe1927"
# 試験の投稿者の login → GitHub の user id。kamonabe とループ用アカウントは本物の id にする
IDS = {OWNER: 19951033, STACK: 348768222}


def u(login):
    return {"login": login, "id": IDS.get(login, 1000)}


def c(author, body, at, updated=None):
    return {"user": u(author), "body": body, "created_at": at, "updated_at": updated or at}


def rv(author, body, at):
    return {"user": u(author), "body": body, "submitted_at": at, "state": "COMMENTED"}


def pr(n, labels=("loop:impl",), comments=(), reviews=(), head="2026-10-28T05:00:00Z", running_at=None):
    return {"number": n, "labels": [{"name": x} for x in labels], "comments": list(comments),
            "reviews": list(reviews), "review_comments": [], "head_committed_at": head,
            "head": {"sha": "abc1234ffff"}, "running_labeled_at": running_at,
            "user": u(STACK), "body": "PR 本文", "created_at": "2026-10-28T04:00:00Z"}


def issue(n, author=OWNER, body="本文", labels=(), comments=(), closed_at=None):
    d = {"number": n, "user": u(author), "body": body, "title": f"t{n}",
         "labels": [{"name": x} for x in labels], "comments": list(comments),
         "created_at": "2026-10-27T00:00:00Z"}
    if closed_at:
        d["closed_at"] = closed_at
    return d


OK_REVIEW = rv(STACK, "🤖 Claude Code review（シニア）abc1234\n...\n結論: 指摘なし", "2026-10-28T06:00:00Z")


class Judge(unittest.TestCase):
    def test_nothing(self):
        r = judge([], [], NOW)
        self.assertTrue(r["nothing_to_do"])

    def test_reviewed_pr_waiting_for_3h_is_nothing(self):
        # 指摘なしのレビュー済みで、最新 commit から 3 時間たっていない PR は拾わない
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T10:00:00Z")
        p["reviews"][0] = rv(STACK, OK_REVIEW["body"], "2026-10-28T10:30:00Z")
        self.assertTrue(judge([p], [], NOW)["nothing_to_do"])

    def test_non_loop_pr_is_ignored(self):
        p = pr(1, labels=(), comments=[c(OWNER, "承認: マージしてください", "2026-10-28T11:00:00Z")])
        self.assertTrue(judge([p], [], NOW)["nothing_to_do"])

    def test_owner_comment_after_last_agent_write(self):
        p = pr(1, reviews=[OK_REVIEW], comments=[
            c(STACK, "🤖 Claude Code: 着手しました", "2026-10-28T07:00:00Z"),
            c(OWNER, "修正依頼: 名前を変えて", "2026-10-28T08:00:00Z")])
        f = judge([p], [], NOW)["found"]
        self.assertEqual([x["pr"] for x in f["owner_comments_on_loop_prs"]], [1])
        # kamonabe のコメントが最新 commit の後にあるので自動マージ候補にはしない
        self.assertEqual(f["automerge_candidates"], [])

    def test_owner_comment_already_answered_is_not_picked(self):
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T10:00:00Z", comments=[
            c(OWNER, "これは何？", "2026-10-28T07:00:00Z"),
            c(STACK, "🤖 Claude Code: 回答します", "2026-10-28T07:10:00Z")])
        p["reviews"][0] = rv(STACK, OK_REVIEW["body"], "2026-10-28T10:30:00Z")
        self.assertTrue(judge([p], [], NOW)["nothing_to_do"])

    def test_agent_style_comment_from_owner_account_is_not_instruction(self):
        p = pr(1, reviews=[rv(STACK, OK_REVIEW["body"], "2026-10-28T10:30:00Z")],
               head="2026-10-28T10:00:00Z",
               comments=[c(OWNER, "🤖 Kiro: 承認: マージしてください", "2026-10-28T11:00:00Z")])
        self.assertTrue(judge([p], [], NOW)["nothing_to_do"])

    def test_running_label(self):
        fresh = pr(1, labels=("loop:impl", "loop:running"), running_at="2026-10-28T11:00:00Z",
                   comments=[c(OWNER, "修正依頼: x", "2026-10-28T11:30:00Z")])
        stale = pr(2, labels=("loop:design", "loop:running"), running_at="2026-10-28T09:00:00Z")
        f = judge([fresh, stale], [], NOW)["found"]
        self.assertEqual([x["pr"] for x in f["stale_running"]], [2])
        # 目印の付いた PR のコメントは次の回に回す
        self.assertEqual(f["owner_comments_on_loop_prs"], [])

    def test_judgement_wait(self):
        wait = c(STACK, "🤖 Claude Code 判断待ち: Slack https://kamo-dev.slack.com/archives/C1/p1",
                 "2026-10-28T09:00:00Z")
        p = pr(1, comments=[wait])
        r = judge([p], [], NOW)
        self.assertFalse(r["nothing_to_do"])
        t = r["slack_threads_to_read"][0]
        self.assertEqual(t["links"], ["https://kamo-dev.slack.com/archives/C1/p1"])
        self.assertTrue(t["over_2h"])
        self.assertFalse(t["resolved"])
        # 未回答の判断待ちがある PR は再レビューの対象にしない
        self.assertEqual(r["found"]["rereview_candidates"], [])

    def test_resolved_wait_on_open_issue_still_reads_thread(self):
        i = issue(5, author=STACK, labels=("loop:drift",), comments=[
            c(STACK, "🤖 Claude Code 判断待ち: Slack https://x.slack.com/archives/C1/p2", "2026-10-28T01:00:00Z"),
            c(STACK, "🤖 Claude Code: 返信なしのため推奨案で確定: Q1=A", "2026-10-28T03:00:00Z")])
        r = judge([], [i], NOW)
        self.assertEqual(len(r["slack_threads_to_read"]), 1)
        self.assertTrue(r["slack_threads_to_read"][0]["resolved"])
        self.assertFalse(r["slack_threads_to_read"][0]["over_2h"])

    def test_rereview_candidate_after_new_commit(self):
        # 指摘なしのレビューより後に commit が足された
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T07:00:00Z")
        f = judge([p], [], NOW)["found"]
        self.assertEqual([x["pr"] for x in f["rereview_candidates"]], [1])
        self.assertEqual(f["automerge_candidates"], [])

    def test_automerge_candidate(self):
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T05:00:00Z")
        f = judge([p], [], NOW)["found"]
        self.assertEqual([x["pr"] for x in f["automerge_candidates"]], [1])

    def test_hold_blocks_automerge(self):
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T05:00:00Z",
               comments=[c(OWNER, "保留: 様子を見る", "2026-10-28T04:00:00Z"),
                         c(STACK, "🤖 Claude Code: 了解", "2026-10-28T04:10:00Z")])
        self.assertTrue(judge([p], [], NOW)["nothing_to_do"])

    def test_owner_is_matched_by_user_id_not_login(self):
        # 旧 login `kamonabe` は別人が取れるので、id が違えば指示にも仕分けの対象にもしない
        other = {"login": "kamonabe", "id": 999}
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T05:00:00Z",
               comments=[{"user": other, "body": "承認: マージしてください",
                          "created_at": "2026-10-28T11:00:00Z", "updated_at": "2026-10-28T11:00:00Z"}])
        i = issue(50)
        i["user"] = other
        r = judge([p], [i], NOW)["found"]
        self.assertEqual(r["issues_to_triage"], [])
        self.assertEqual(r["owner_instructions_after_head"], [])
        self.assertEqual([x["pr"] for x in r["automerge_candidates"]], [1])
        # kamonabe の id なら、login が何であっても kamonabe の書き込みとして扱う
        hold = {"user": {"login": "kamonabe", "id": 19951033}, "body": "保留: 待って",
                "created_at": "2026-10-28T04:00:00Z", "updated_at": "2026-10-28T04:00:00Z"}
        p2 = pr(2, reviews=[OK_REVIEW], head="2026-10-28T05:00:00Z", comments=[hold])
        self.assertEqual(judge([p2], [], NOW)["found"]["automerge_candidates"], [])

    def test_stuck_pr_is_not_candidate(self):
        p = pr(1, labels=("loop:impl", "loop:stuck"))
        self.assertTrue(judge([p], [], NOW)["nothing_to_do"])

    def test_issue_triage(self):
        new = issue(10)
        triaged = issue(11, labels=("loop:triaged",))
        agent = issue(12, body="🤖 Claude Code: ...")
        other = issue(13, author="someone")
        f = judge([], [new, triaged, agent, other], NOW)["found"]
        self.assertEqual([x["issue"] for x in f["issues_to_triage"]], [10])

    def test_owner_answer_on_triaged_and_needs_design(self):
        a = issue(20, labels=("loop:triaged",), comments=[
            c(STACK, "🤖 Claude Code（シニア）仕分け: 聞き返し", "2026-10-28T01:00:00Z"),
            c(OWNER, "こういう意味です", "2026-10-28T02:00:00Z")])
        b = issue(21, author=STACK, body="🤖 止まりました", labels=("needs-design",), comments=[
            c(OWNER, "B で", "2026-10-28T02:00:00Z")])
        f = judge([], [a, b], NOW)["found"]
        self.assertEqual([x["issue"] for x in f["owner_comments_on_issues"]], [20, 21])

    # ---- レビューでの指摘を受けて足した試験

    def test_instruction_written_while_running_is_not_lost(self):
        # 目印の付いている間に承認が書かれ、そのあとエージェントが書き込んだ
        p = pr(1, reviews=[OK_REVIEW], head="2026-10-28T05:00:00Z", comments=[
            c(OWNER, "承認: マージしてください", "2026-10-28T06:30:00Z"),
            c(STACK, "🤖 Claude Code: loop:running を外しました", "2026-10-28T07:00:00Z")])
        r = judge([p], [], NOW)
        self.assertFalse(r["nothing_to_do"])
        self.assertEqual([x["pr"] for x in r["found"]["owner_instructions_after_head"]], [1])

    def test_wait_in_issue_body(self):
        i = issue(30, author=STACK, labels=("needs-design",),
                  body="🤖 Claude Code: 止まりました。判断待ち: Slack https://x.slack.com/archives/C1/p1793000000000001")
        r = judge([], [i], NOW)
        self.assertEqual(len(r["slack_threads_to_read"]), 1)
        self.assertFalse(r["slack_threads_to_read"][0]["resolved"])

    def test_owner_reply_on_drift_issue(self):
        i = issue(31, author=STACK, body="🤖 整合点検", labels=("loop:drift",), comments=[
            c(STACK, "🤖 Claude Code 判断待ち: Slack https://x.slack.com/archives/C1/p1793000000000002", "2026-10-28T01:00:00Z"),
            c(OWNER, "判断: B", "2026-10-28T01:30:00Z")])
        f = judge([], [i], NOW)["found"]
        self.assertEqual([x["issue"] for x in f["owner_comments_on_issues"]], [31])

    def test_recently_closed_item_threads_are_read(self):
        i = issue(32, author=STACK, body="🤖 PR 本文", closed_at="2026-10-27T12:00:00Z", comments=[
            c(STACK, "🤖 Claude Code 判断待ち: Slack https://x.slack.com/archives/C1/p1793000000000003", "2026-10-27T01:00:00Z"),
            c(STACK, "🤖 Claude Code: 返信なしのため推奨案で確定: Q1=A（https://x.slack.com/archives/C1/p1793000000000003）",
              "2026-10-27T03:00:00Z")])
        i["pull_request"] = {}
        r = judge([], [], NOW, closed=[i])
        self.assertEqual(len(r["slack_threads_to_read"]), 1)
        self.assertTrue(r["slack_threads_to_read"][0]["resolved"])

    def test_wait_variants(self):
        full_width = c(STACK, "🤖 Claude Code 判断待ち：Slack https://x.slack.com/archives/C1/p1793000000000004",
                       "2026-10-28T09:00:00Z")
        no_link = c(STACK, "🤖 Claude Code 判断待ち: Slack （リンク取得に失敗）", "2026-10-28T09:00:00Z")
        r = judge([], [issue(40, author=STACK, body="🤖", comments=[full_width]),
                       issue(41, author=STACK, body="🤖", comments=[no_link])], NOW)
        self.assertEqual(len(r["slack_threads_to_read"]), 1)
        self.assertEqual([x["where"] for x in r["found"]["waits_without_link"]], ["issue #41"])

    def test_resolution_of_other_thread_does_not_resolve_this_one(self):
        i = issue(42, author=STACK, body="🤖", comments=[
            c(STACK, "🤖 判断待ち: Slack https://x.slack.com/archives/C1/p1793000000000005", "2026-10-28T01:00:00Z"),
            c(STACK, "🤖 判断待ち: Slack https://x.slack.com/archives/C1/p1793000000000006", "2026-10-28T02:00:00Z"),
            c(STACK, "🤖 Claude Code: Slack で kamonabe が選択: Q1=A（https://x.slack.com/archives/C1/p1793000099000000?thread_ts=1793000000.000006）",
              "2026-10-28T03:00:00Z")])
        t = judge([], [i], NOW)["slack_threads_to_read"]
        self.assertEqual([x["resolved"] for x in t], [False, True])
        self.assertIn("1793000099.000000", t[1]["referenced_ts"])

    def test_edited_owner_comment_counts_at_edit_time(self):
        p = pr(1, reviews=[rv(STACK, OK_REVIEW["body"], "2026-10-28T10:30:00Z")], head="2026-10-28T10:00:00Z",
               comments=[c(OWNER, "修正依頼: やはり直して", "2026-10-28T07:00:00Z", updated="2026-10-28T11:00:00Z"),
                         c(STACK, "🤖 Claude Code: 回答", "2026-10-28T08:00:00Z")])
        self.assertFalse(judge([p], [], NOW)["nothing_to_do"])

    def test_template_line_is_not_no_findings(self):
        bad = rv(STACK, "🤖 Claude Code review（シニア）abc1234\n結論: 指摘なし ／ 結論: 指摘あり", "2026-10-28T06:00:00Z")
        p = pr(1, reviews=[bad])
        self.assertEqual([x["pr"] for x in judge([p], [], NOW)["found"]["rereview_candidates"]], [1])

    def test_review_of_older_commit_does_not_count(self):
        old = rv(STACK, "🤖 Claude Code review（シニア）0000000\n結論: 指摘なし", "2026-10-28T06:00:00Z")
        p = pr(1, reviews=[old])
        self.assertEqual([x["pr"] for x in judge([p], [], NOW)["found"]["rereview_candidates"]], [1])


if __name__ == "__main__":
    unittest.main()
