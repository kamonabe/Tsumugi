# Tsumugi（紡ぎ）

Rust で作るミニプログラミング言語。言語処理系の学習を目的としたプロジェクト。

## 概要

Tsumugi は Ruby 風の文法を持つ動的型付けインタプリタ言語。
ソースコードを レキサー → パーサー → 評価器 の3段パイプラインで実行する。

## クイックスタート

```bash
# ビルド
cargo build

# ファイル実行
cargo run -- examples/hello.tsg

# REPL（対話モード）
cargo run
```

## サンプルコード

```
let name = "tsumugi"
print("hello, " + name)

fn add(a, b)
    return a + b
end

print(add(3, 4))

let count = 3
while count > 0
    print(count)
    let count = count - 1
end
```

## 対応機能

- 整数・浮動小数点・文字列・真偽値・null
- 変数束縛 (`let`)
- 四則演算・文字列結合
- 比較演算 (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- 論理演算 (`and`, `or`, `not`)
- 条件分岐 (`if` / `else` / `end`)
- 関数定義・呼び出し (`fn` / `return` / `end`)
- while ループ
- REPL（複数行入力対応）
- ファイル実行

## プロジェクト構成

```
src/
├── main.rs     # エントリポイント（REPL / ファイル実行）
├── token.rs    # トークン型定義
├── lexer.rs    # レキサー（ソース → トークン列）
├── ast.rs      # AST ノード定義
├── parser.rs   # パーサー（トークン列 → AST）
├── value.rs    # 実行時の値型
├── env.rs      # 環境（変数スコープ・関数テーブル）
└── eval.rs     # 評価器（AST → 実行）
```

## ライセンス

MIT
