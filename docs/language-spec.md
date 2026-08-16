# Tsumugi 言語仕様

バージョン: 0.2

## コメント

`#` から行末まで。

```
# これはコメント
let x = 10  # ここもコメント
```

## データ型

| 型 | リテラル例 |
|---|---|
| 整数 | `0`, `42`, `-3` |
| 浮動小数点 | `3.14`, `0.5` |
| 文字列 | `"hello"`, `"line\n"` |
| 真偽値 | `true`, `false` |
| null | `null` |
| リスト | `[1, "two", true]` |
| 辞書 | `{"key": value}` |

### 文字列エスケープ

| エスケープ | 意味 |
|---|---|
| `\n` | 改行 |
| `\t` | タブ |
| `\\` | バックスラッシュ |
| `\"` | ダブルクォート |

### リスト

```
let xs = [1, 2, 3]
let empty = []
let mixed = [1, "two", true, null]
let nested = [[1, 2], [3, 4]]
```

- 任意の型の値を混在して格納できる
- ネスト可能
- 末尾カンマ許容: `[1, 2, 3,]`

### 辞書

```
let d = {"name": "tsumugi", "version": 1}
let empty = {}
```

- キーは文字列のみ
- 値は任意の型
- 末尾カンマ許容: `{"a": 1, "b": 2,}`
- キーの順序はアルファベット順で保持される（BTreeMap）

## インデックスアクセス

リスト・辞書・文字列に対して `[...]` でアクセスできる。

### リストのインデックス

```
let xs = [10, 20, 30]
print(xs[0])    # 10
print(xs[-1])   # 30（末尾から）
```

- 0始まり
- 負のインデックスで末尾からアクセス（`-1` が最後の要素）
- 範囲外アクセスはランタイムエラー

### 辞書のキーアクセス

```
let d = {"name": "tsumugi"}
print(d["name"])     # tsumugi
print(d["missing"])  # null（存在しないキーは null を返す）
```

### 文字列のインデックス

```
let s = "hello"
print(s[0])   # h
print(s[-1])  # o
```

> **性能上の注意**: 文字列のインデックスアクセスは内部的に先頭から文字を数えるため、文字列の長さに比例したコストがかかる（O(n)）。短い文字列では問題にならないが、長い文字列に対してループでインデックスアクセスを繰り返すと O(n²) になる。文字を順に処理したい場合は `for ch in str ... end` を使う方が効率的。

### インデックス代入

```
let xs = [1, 2, 3]
xs[0] = 99
print(xs)  # [99, 2, 3]

let d = {"a": 1}
d["b"] = 2         # 新しいキーの追加
d["a"] = 10        # 既存キーの更新
```

> **制約**: インデックス代入の対象は変数に直接格納されたリストまたは辞書のみ。ネストしたインデックス代入（`xs[0][1] = val`）は現在サポートされていない。ネストした構造を更新するには、一時変数に取り出して代入し直す。

## 変数

### 宣言（let）

```
let x = 10
let name = "tsumugi"
let flag = true
```

### 再代入

`let` で宣言済みの変数に対して、`let` なしで値を更新できる。

```
let count = 5
count = count - 1
print(count)  # 4
```

- 再代入は既存の変数を検索し、見つかったスコープ上で値を上書きする
- 内側のスコープから外側の変数も更新可能
- 未宣言の変数に代入しようとするとランタイムエラーになる

```
x = 42  # エラー: 1行目: 未定義の変数に代入: x
```

## 演算子

### 算術

| 演算子 | 意味 |
|---|---|
| `+` | 加算 / 文字列結合 |
| `-` | 減算 / 単項マイナス |
| `*` | 乗算 |
| `/` | 除算（整数同士は整数除算） |
| `%` | 剰余（整数同士は整数、ゼロ除算はエラー） |

### 比較

| 演算子 | 意味 |
|---|---|
| `==` | 等しい |
| `!=` | 等しくない |
| `<` | 小さい |
| `>` | 大きい |
| `<=` | 以下 |
| `>=` | 以上 |

### 論理

| 演算子 | 意味 |
|---|---|
| `and` | 論理積 |
| `or` | 論理和 |
| `not` | 論理否定 |

### 優先順位（低→高）

1. `or`
2. `and`
3. `==` `!=` `<` `>` `<=` `>=`
4. `+` `-`
5. `*` `/` `%`
6. `not` `-`（単項）
7. 関数呼び出し

## 条件分岐

```
if 条件
    本体
end

if 条件
    本体
else
    本体
end

if 条件
    本体
elif 条件
    本体
elif 条件
    本体
else
    本体
end
```

`elif` はいくつでも連鎖できる。`else` は省略可能。

## ループ

### while

```
while 条件
    本体
end
```

### for

リスト・辞書・文字列を反復する。

```
for item in collection
    本体
end
```

対応するコレクション:
- リスト — 各要素を順にバインド
- 辞書 — キーをアルファベット順にバインド
- 文字列 — 各文字を順にバインド

```
let fruits = ["apple", "banana", "cherry"]
for fruit in fruits
    print(fruit)
end

let total = 0
for n in [1, 2, 3, 4, 5]
    total = total + n
end
print(total)  # 15
```

### break / continue

`break` はループを即座に抜ける。`continue` は現在のイテレーションをスキップして次へ進む。

```
# 5で停止
let i = 0
while true
    if i == 5
        break
    end
    i = i + 1
end

# 3をスキップ
for n in [1, 2, 3, 4, 5]
    if n == 3
        continue
    end
    print(n)
end
```

- `while` / `for` のどちらでも使用可能
- ループの外で使うとランタイムエラー

## 関数

```
fn 関数名(引数1, 引数2)
    return 式
end
```

呼び出し:

```
let result = add(3, 4)
```

### 第一級関数

関数は値として扱える。変数に代入したり、別の関数に引数として渡したりできる。

```
fn double(x)
    return x * 2
end

let f = double
print(f(3))       # 6
print(type(f))    # fn
```

### 無名関数（ラムダ）

名前のない関数を式として書ける。

複数行:
```
let add = fn(a, b)
    return a + b
end
print(add(1, 2))  # 3
```

1行（`return` 省略可）:
```
let square = fn(x) x * x end
print(square(5))  # 25
```

1行で `return` を明示してもよい:
```
let neg = fn(x) return -x end
```

### クロージャ

無名関数は定義時のスコープにある変数を捕捉（キャプチャ）する。

```
fn make_adder(n)
    return fn(x) return x + n end
end

let add5 = make_adder(5)
print(add5(3))   # 8
print(add5(10))  # 15
```

キャプチャは**値コピー**で行われる。定義時点の値がコピーされるため、後から元の変数を変更してもクロージャ内の値は変わらない。

```
let base = 10
let adder = fn(x) return x + base end
base = 999
print(adder(1))  # 11（base=10 の時点のコピーを使う）
```

> **制約**: キャプチャした変数への再代入は元のスコープに反映されない（値キャプチャ方式）。カウンターのような「状態を保持するクロージャ」は現在サポートされていない。

### 高階関数

関数を引数として受け取る関数を書ける。

```
fn apply(func, val)
    return func(val)
end

print(apply(fn(x) x * 3 end, 7))  # 21
```

組み込み高階関数 `map` / `filter` / `each`:

```
let nums = [1, 2, 3, 4, 5]

# map: 各要素を変換して新しいリストを返す
let doubled = map(nums, fn(x) x * 2 end)
print(doubled)  # [2, 4, 6, 8, 10]

# filter: 条件を満たす要素だけ残す
let evens = filter(nums, fn(x) x % 2 == 0 end)
print(evens)  # [2, 4]

# each: 各要素に処理を実行（戻り値なし）
each(nums, fn(x) print(x) end)
```

## 組み込み関数

| 関数 | 説明 |
|---|---|
| `print(値, ...)` | 値を標準出力に表示。複数引数はスペース区切りで出力 |
| `len(x)` | 文字列・リスト・辞書の長さを返す |
| `push(list, val)` | リストの末尾に値を追加（破壊的操作） |
| `pop(list)` | リストの末尾の値を取り出して返す（破壊的操作） |
| `keys(dict)` | 辞書のキー一覧をリストで返す |
| `type(x)` | 値の型名を文字列で返す |
| `slice(collection, start, end)` | リスト/文字列の部分を切り出す（start から end の手前まで） |
| `contains(collection, val)` | リスト/辞書/文字列に値が含まれていれば true |
| `split(str, sep)` | 文字列をセパレータで分割しリストで返す |
| `join(list, sep)` | リストの各要素をセパレータで結合して文字列で返す |
| `to_int(val)` | 値を整数に変換（文字列/Float/Bool対応） |
| `to_str(val)` | 値を文字列に変換 |
| `range(start, end)` | start から end の手前までの整数リストを生成 |
| `read_file(path)` | ファイル全体を文字列で返す。失敗時は null |
| `read_lines(path)` | ファイルを行ごとに読んでリストで返す。失敗時は null |
| `write_file(path, content)` | ファイルに書き込み（上書き）。成功で true、失敗で false |
| `append_file(path, content)` | ファイルに追記。成功で true、失敗で false |
| `env(name)` | 環境変数を取得。未設定なら null |
| `args()` | コマンドライン引数をリストで返す（スクリプトパスは含まない） |
| `input()` | 標準入力から1行読み取る。EOF なら null |
| `now()` | 現在のUNIXタイムスタンプ（秒）を整数で返す |
| `format_time(timestamp, format)` | タイムスタンプをフォーマット（%Y, %m, %d, %H, %M, %S） |
| `path_exists(path)` | パスが存在すれば true |
| `path_join(parts...)` | パーツを結合してパス文字列を返す |
| `mkdir(path)` | ディレクトリを再帰的に作成。成功で true |
| `remove(path)` | ファイルまたは空ディレクトリを削除。成功で true |
| `remove_dir(path)` | ディレクトリを中身ごと再帰削除。成功で true |
| `rename(from, to)` | ファイル/ディレクトリを移動・リネーム。成功で true |
| `list_dir(path)` | ディレクトリ内のエントリ名をリストで返す。失敗で null |
| `file_size(path)` | ファイルサイズ（バイト）を整数で返す。失敗で null |
| `trim(str)` | 前後の空白を除去 |
| `starts_with(str, prefix)` | 接頭辞チェック |
| `ends_with(str, suffix)` | 接尾辞チェック |
| `replace(str, old, new)` | 文字列置換（全出現箇所） |
| `upper(str)` | 大文字に変換 |
| `lower(str)` | 小文字に変換 |
| `to_float(val)` | 浮動小数点に変換 |
| `abs(num)` | 絶対値 |
| `min(a, b)` | 小さい方を返す |
| `max(a, b)` | 大きい方を返す |
| `sort(list)` | リストをソートして返す（文字列表現で比較） |
| `reverse(list or str)` | リストまたは文字列を反転して返す |
| `is_file(path)` | パスがファイルなら true |
| `is_dir(path)` | パスがディレクトリなら true |
| `values(dict)` | 辞書の値一覧をリストで返す（キーのアルファベット順） |
| `has_key(dict, key)` | 辞書にキーが存在すれば true（値が null でも true を返す） |
| `floor(num)` | 小数点以下を切り捨てて整数で返す |
| `ceil(num)` | 小数点以下を切り上げて整数で返す |
| `round(num)` | 四捨五入して整数で返す |
| `exit(code?)` | プロセスを終了する。引数省略時は終了コード 0 |
| `map(list, fn)` | リストの各要素に関数を適用し、結果のリストを返す |
| `filter(list, fn)` | リストの各要素に関数を適用し、真を返した要素のリストを返す |
| `each(list, fn)` | リストの各要素に関数を適用する（戻り値なし、副作用用途） |

## 真偽判定

以下は偽（falsy）として扱われる:
- `false`
- `null`
- `0`（整数のゼロ）
- `0.0`（浮動小数点のゼロ）
- `""`（空文字列）
- `[]`（空リスト）
- `{}`（空辞書）

それ以外はすべて真（truthy）。

## エラーメッセージ

パースエラー・ランタイムエラーともに行番号が付与される。
関数内で発生したエラーにはスタックトレース（呼び出し経路）が追加表示される。

```
2行目: let の後に識別子が必要です。got: Assign
3行目: 未定義の変数: z
1行目: 未定義の変数に代入: x
1行目: ゼロ除算
3行目: 型エラー: Str("hello") Add Int(1) は計算できません
2行目: インデックス範囲外: 5 (長さ: 3)
1行目: 辞書のキーは文字列である必要があります
```

スタックトレース例:

```
2行目: ゼロ除算
  in divide() (6行目)
  in calc() (9行目)
```

`in 関数名() (N行目)` はその関数が呼び出された行を示す。内側（エラー発生地点に近い方）から外側の順に表示される。
