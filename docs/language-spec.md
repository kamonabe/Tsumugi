# Tsumugi 言語仕様

バージョン: 0.7

最終更新: 2026-08-28

この番号は言語仕様のrevisionであり、Cargo package / REPLの実装バージョン `0.1.0` とは独立して管理する。

本書をTsumugiの規範仕様とし、デフォルトのツリーウォーク版と`--vm`版は同じ観測可能な挙動を満たすものとする。実装差は言語機能ではなく非適合として扱い、詳細と修正状況は[`roadmap.md`](roadmap.md)の監査バックログで管理する。

### 現在の既知非適合

現行実装には、比較対象型、同一scopeでの再宣言、captured collectionへのindex代入、top-level importの評価時点、未捕捉REPLエラー後の状態に両engine差がある。また、`path_join`の非文字列argument無視は両engine共通の仕様違反である。これらの挙動へ依存するコードは移植可能とみなさない。

本書の文法どおり、複数行lambdaの終端`end`は必須である。

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

### f-string（文字列補間）

`f"..."` プレフィックスを付けた文字列リテラルでは、`{式}` の中に任意の式を埋め込める。
式の結果は自動的に文字列に変換される（`to_str()` 相当）。

```
let name = "world"
print(f"hello, {name}")        # hello, world
print(f"2 + 3 = {2 + 3}")     # 2 + 3 = 5

let items = [1, 2, 3]
print(f"items: {items}")       # items: [1, 2, 3]

let flag = true
print(f"flag: {flag}")         # flag: true
```

#### ブレースのエスケープ

リテラルの `{` や `}` を出力したい場合は二重にする。

```
print(f"use {{braces}} like this")  # use {braces} like this
```

#### 式の中で使えるもの

- 変数参照: `{x}`
- 算術・比較式: `{x + y}`, `{x > 0}`
- 関数呼び出し: `{len(s)}`
- インデックスアクセス: `{xs[0]}`
- 文字列リテラル: `{replace(s, "a", "b")}`

#### 通常文字列との違い

| 構文 | 補間 | `{` の扱い |
|---|---|---|
| `"..."` | なし | そのまま文字として使える |
| `f"..."` | あり | `{{` でエスケープが必要 |

#### エラーケース

- 空の式 `f"hello {}"` → パースエラー
- 閉じられていない式 `f"hello {x"` → パースエラー
- 未閉じの f-string `f"hello` → パースエラー

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

**評価順**: `target[index] = value` は左から右へ、次の順で処理する。

1. `target` の変数を解決する。未定義なら、この時点で「未定義の変数」エラーになる（`index` と `value` は評価しない）
2. `index` を評価する
3. `value` を評価する
4. 解決済みの変数が保持するコレクションを直接更新する

対象はローカル変数・クロージャがキャプチャした変数・トップレベル変数のいずれでもよい。更新はコレクション全体の書き戻しではなく直接更新のため、`index` や `value` の評価中に同じ変数へ加えられた変更は保持され、境界判定もその最新状態に対して行われる。

```
let xs = [1, 2, 3]
fn touch()
    xs[1] = 20     # value 評価中の変更も残る
    return 10
end
xs[0] = touch()
print(xs)          # [10, 20, 3]
```

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

### ブロックスコープ

以下はそれぞれ独立したレキシカルスコープを作る。

- 実行対象に選ばれた各 `if` / `elif` / `else` ブロック
- `while` / `for` の各イテレーション
- `try` ブロックと `catch` ブロック（両者は別スコープ）
- 関数本体

`let` は現在のスコープに新しい変数を宣言し、同名の外側変数があればshadowする。`let`を付けない代入は、最も内側にある既存の変数を検索して更新する。

```
let result = null
if true
    let temporary = "block local"
    result = temporary
end

print(result)     # block local
# print(temporary)  # エラー: block外では未定義
```

- `for` のループ変数は各イテレーションの開始時に新しい変数セルへbindされる。異なるイテレーションで作られたクロージャは別々のセルを保持し、同一イテレーション内で作られたクロージャと通常代入は同じセルを共有する
- `try` 内の `let` は `catch` から参照できない。値を渡す場合は外側で変数を宣言し、`try` 内で代入する
- `catch` 変数と `catch` 内の `let` は、その `catch` ブロック内だけで有効
- scopeは正常終了、ランタイムエラー、`return`、`break`、`continue`のどの経路でも解放される
- block localをキャプチャしたクロージャが外へ保存された場合、変数名はblock外から参照できないが、キャプチャされた変数セルはクロージャが保持する
- 正常終了または同一実行内でcatchされたエラーでは、scope解放自体はトランザクションrollbackを行わない。外側変数への代入、外側コレクションの変更、外部I/Oは保持される
- 未捕捉エラーで終了したREPL入力のcommit/rollback方針は両engineで未統一であり、AUD-024で継続する

### 名前の可視性と解決時期

- local変数と定義時に見えていた外側bindingは、通常のレキシカルスコープで解決する
- 変数・関数名の存在は、その名前を含む式または代入文を**実行した時点**で検査する。到達しない`if` / loop / `catch`、短絡された`and` / `or`の右辺、未呼出し関数、`return`後の文に未定義名があってもエラーにならない
- top-levelの`let`と関数定義は、その宣言を実行した時点からglobalとして見える。宣言前のtop-level文から直接読むhoistingは行わない
- 関数・ラムダは、定義時に存在したレキシカルbindingを変数セルとして捕捉する。定義時に未解決だった名前は呼出し時のglobalを参照するため、globalの宣言後に呼び出せばforward referenceとtop-level mutual recursionが可能
- 定義後に作られたblock localはforward referenceの対象にならない。block localを関数から使うには、関数定義時にそのbindingが見えている必要がある
- importされたtop-level定義も同じglobal scopeへ入り、caller側で後から宣言され、呼出し前に実行済みとなったglobalを参照できる
- 実行時にも名前が存在しなければ`name`ランタイムエラーとなり、`try` / `catch`で捕捉できる。未定義calleeでは引数を評価しない

```
fn read_later()
    return later
end

let later = 42
print(read_later())  # 42: 呼出し前にglobal宣言が実行済み

# print(not_yet_defined)  # runtime error: この文の実行時点では未定義
# let not_yet_defined = 1
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

- `return`は関数定義または複数行ラムダの本体でのみ使用できる。本体内であれば、条件分岐・ループ・`try` / `catch`のブロックからも使用できる
- 関数の外に`return`を書くと、実行前にパースエラー `return は関数の中でのみ使用できます` になる。トップレベル、トップレベルのblock内、import先ファイルのトップレベルはいずれも関数の外である
- 1行ラムダ（`fn(x) x * 2 end`）では`return`を省略でき、`fn(x) return x * 2 end`と明示しても同じ意味になる
- `return`のない関数、および`return`に到達せずに本体が終わった関数は`null`を返す

呼び出し:

```
let result = add(3, 4)
```

### ユーザー定義関数の呼び出し順序

ユーザー定義関数の呼び出しは、次の順で処理する。

1. ステップ予算とcall depthを検査する
2. callee式を評価する
3. calleeが関数であり、引数の個数が一致することを検査する
4. 引数を左から右へ評価する
5. 関数bodyを実行する

ステップ予算・call depthの検査に失敗した場合、callee式と引数は評価しない。callee式の評価に失敗した場合も引数は評価しない。calleeが関数ではない場合、または引数の個数が一致しない場合、引数と関数bodyは評価しない。引数の評価中にエラーが発生した場合は、その時点で残りの引数と関数bodyを実行しない。

識別子をcalleeとして呼ぶ場合、`print`を除いて、現在のlocal・parameter・self-binding、captured binding、runtime globalの順でユーザーbindingを探す。bindingが存在するときはbuiltinと同名でもその値をcalleeとし、関数でなければbuiltinへfallbackせずエラーにする。bindingが存在せず、識別子がbuiltin名である場合だけbuiltinとして呼び出す。`print`は予約tokenであり、常にbuiltinとして扱う。

上記のユーザー関数呼び出し順序は、builtinとユーザーbindingの選択後にユーザー関数が選ばれた場合の契約である。組み込み関数は、破壊的更新やcallbackなど各関数固有の契約に従う。

### コンテキスト依存組み込み関数の呼び出し順序

`input`、`args`、`exit`、`push`、`pop`、`map`、`filter`、`each`は、ユーザーbindingよりbuiltinが選ばれた後、次の順で処理する。

1. 引数の個数を検査する
2. `push` / `pop`では、第1引数のsource式が識別子であることを検査する
3. 引数を左から右へ評価する
4. 評価済みの値について型・状態を検査し、builtin本体を実行する

引数個数が不正な場合、または`push` / `pop`の第1引数が識別子でない場合、引数式は一つも評価しない。前者は`argument`、後者は`builtin_type`エラーとする。`print`は可変長引数を左から右へ評価する。その他の共通builtinは、引数を左から右へ評価してから関数固有の個数・型検査を行う。

`push` / `pop`の第1引数は、実行時に存在するList bindingでなければならない。local、captured binding、runtime globalを使用できるが、List literal、index式、関数の戻り値などの一時値は指定できない。`push`は第1引数の値を読み取ってから第2引数を評価し、そのsnapshotへ値を追加して同じbindingへ書き戻し、`null`を返す。第2引数の評価中に同じbindingが再代入・変更されても、最終的には先に読み取ったsnapshotを基にしたListを書き戻す。`pop`は末尾要素を除いたListを書き戻し、取り出した要素を返す。非List bindingと空Listからの`pop`は`builtin_type`エラーとする。

`map` / `filter` / `each`は外側の2引数を左から右へ評価した後で、第1引数がListか検査する。callbackのcallable・arity検査は各要素を処理するときに行い、空Listではcallbackを呼び出さない。

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

### 名前付き関数のself-binding

名前付き関数のbodyでは、宣言名を現在呼び出している関数値へ束縛する。トップレベルかローカルかに関係なく、関数を別名へ代入した場合や外側の関数から返した場合も、body内の宣言名で自身を再帰呼び出しできる。

```
fn outer()
    fn factorial(n)
        if n <= 1
            return 1
        end
        return n * factorial(n - 1)
    end
    return factorial
end

let fact = outer()
print(fact(5))  # 120
```

self-bindingはcaptured bindingより優先し、同名のparameterがある場合はparameterがself-bindingをshadowする。無名関数には暗黙のself名を導入しない。

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

キャプチャは**変数セルの参照共有**で行われる。定義後に元の変数を再代入するとクロージャからも新しい値が見え、クロージャ内の再代入も元の変数へ反映される。

```
let base = 10
let adder = fn(x) return x + base end
base = 999
print(adder(1))  # 1000
```

この仕組みにより、状態を保持するクロージャも作成できる。

```
fn make_counter()
    let n = 0
    return fn()
        n = n + 1
        return n
    end
end

let counter = make_counter()
print(counter())  # 1
print(counter())  # 2
```

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

## モジュール / import

別のファイルに定義された関数や変数を読み込むことができる。

```
import "path/to/module.tsg"
```

### 基本的な使い方

```
# math.tsg
fn square(x)
    return x * x
end

fn cube(x)
    return x * x * x
end
```

```
# main.tsg
import "math.tsg"

print(square(5))   # 25
print(cube(3))     # 27
```

### パス解決

- パスは現在実行中のスクリプトからの相対パスで解決される
- ネストした import（A が B を import し、B が C を import する）にも対応

### 循環 import

- 同じファイルが正常に import 済みの場合、2回目以降はスキップされる（エラーにはならない）
- 読み込み・パース・実行に失敗した import は完了扱いにせず、同じパスを再試行できる
- これにより循環 import が安全に処理される

### 制約

- import はプログラムのトップレベルでのみ使用できる。関数・条件分岐・ループ・`try` / `catch`・複数行ラムダのブロック内に記述すると、パスの読み込み前にパースエラー `import はトップレベルでのみ使用できます` になる
- root scriptを数えず、現在処理中のimport chainは最大128ファイル。異なるファイルへの129段目のimportは`import 失敗: ネストが深すぎます (上限: 128)`という`import`ランタイムエラーになる。既訪問パスは深度検査前にスキップされるため、循環importの従来挙動は変わらない
- import されたファイル内の `let` 宣言・`fn` 定義がすべて現在のスコープに展開される（名前空間は分離されない）
- REPL での import は現在の作業ディレクトリからの相対パスで解決される
- `TSUMUGI_SANDBOX` が設定されている場合、import 先パスも許可範囲内でなければサンドボックス違反エラーになる

## エラー処理（try / catch）

ランタイムエラーを捕捉して、プログラムを停止させずに処理を続行できる。

```
try
    # エラーが発生する可能性のあるコード
catch 変数名
    # エラーが発生した場合に実行されるコード
    # 変数には構造化Error値がバインドされる
end
```

### 基本的な使い方

```
try
    let result = 10 / 0
catch e
    print("エラー: " + e)   # エラー: ゼロ除算
end
print("続行")               # 続行
```

`catch` の変数 `e` には構造化されたError値がバインドされる。`e["type"]`、`e["message"]`、`e["line"]` で種別・メッセージ・発生行を参照できる。文字列結合やf-stringでは従来どおりエラーメッセージへ自動変換される。

`try`と`catch`は別のブロックスコープであり、`try`内で`let`した変数は`catch`から参照できない。catch変数とcatch内の`let`もcatch終了時に破棄される。両ブロック間またはblock外へ値を渡す場合は、外側で変数を宣言して通常代入で更新する。

```
try
    let result = 10 / 0
catch e
    print(e["type"])     # zero_division
    print(e["message"])  # ゼロ除算
    print(e["line"])     # エラー発生行
end
```

### 関数内のエラーをキャッチ

関数の中で発生したエラーも、呼び出し元の try/catch で捕捉できる。

```
fn divide(a, b)
    return a / b
end

try
    let x = divide(10, 0)
catch e
    print(e)   # ゼロ除算
end
```

### ネスト

try/catch はネストできる。内側で捕捉されたエラーは外側には伝播しない。

```
try
    try
        let y = 1 / 0
    catch inner
        print("内側: " + inner)   # 内側: ゼロ除算
    end
    print("外側は続行")           # 外側は続行
catch outer
    print("ここには来ない")
end
```

### エラーがない場合

try ブロック内でエラーが発生しなければ、catch ブロックはスキップされる。

```
try
    let x = 42
    print(x)    # 42
catch e
    print("ここには来ない")
end
```

### キャッチ可能なエラー

以下のランタイムエラーをキャッチできる:

- ゼロ除算
- 型エラー（異なる型同士の演算）
- インデックス範囲外
- 未定義の変数・関数（名前を実際に評価した時点で発生）
- 関数の引数不一致
- リスト/辞書以外へのインデックス代入
- `to_int` の変換失敗
- ステップ上限超過

### キャッチできないエラー

- パースエラー（構文エラー）— プログラムの実行前に発生するため

## 組み込み関数

| 関数 | 説明 |
|---|---|
| `print(値, ...)` | 値を標準出力に表示。複数引数はスペース区切りで出力 |
| `len(x)` | 文字列・リスト・辞書の長さを返す |
| `push(list_variable, val)` | List変数の末尾へ値を追加して同じbindingへ書き戻し、nullを返す（破壊的操作） |
| `pop(list_variable)` | List変数の末尾の値を取り出して返し、残りを同じbindingへ書き戻す（破壊的操作） |
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
| `env(name)` | 環境変数を取得。未設定なら null。`TSUMUGI_ENV_ALLOW` 設定時は許可リスト外のキーも null |
| `args()` | コマンドライン引数をリストで返す（スクリプトパスは含まない） |
| `input()` | 標準入力から1行読み取る。EOF なら null |
| `now()` | 現在のUNIXタイムスタンプ（秒）を整数で返す |
| `format_time(timestamp, format)` | タイムスタンプをフォーマット（%Y, %m, %d, %H, %M, %S） |
| `path_exists(path)` | パスが存在すれば true |
| `path_join(parts...)` | パーツを結合してパス文字列を返す |
| `mkdir(path)` | ディレクトリを再帰的に作成。成功で true |
| `remove(path)` | ファイルまたは空ディレクトリを削除。final symlinkはlink自体だけを削除する。成功で true |
| `remove_dir(path)` | ディレクトリを中身ごと再帰削除。final symlinkはlink自体だけを削除し、targetをたどらない。成功で true |
| `rename(from, to)` | ファイル/ディレクトリを移動・リネーム。from/toのfinal symlinkはtargetでなくdirectory entry自体として扱う。成功で true |
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
1行目: 文字列リテラルが閉じられていません
1行目: 整数リテラルが範囲外です (最大: 9223372036854775807): 99999999999999999999
```

スタックトレース例:

```
2行目: ゼロ除算
  in divide() (6行目)
  in calc() (9行目)
```

`in 関数名() (N行目)` はその関数が呼び出された行を示す。内側（エラー発生地点に近い方）から外側の順に表示される。

## 実行安全性

以下は誤操作や過剰な資源消費を抑えるためのdefense-in-depth機構であり、敵対的なスクリプトを隔離するセキュリティ境界ではない。非信頼コードを実行する場合は、非rootプロセス、権限制限されたコンテナ、read-only filesystem、CPU・メモリ・実行時間上限などOS側の分離を併用する。

構文とASTは深度256、非循環import chainはroot scriptを除いて128に制限し、parser・compiler・evaluatorがhost stackを消費する前にエラー化する。ステップ予算はloop反復と関数・callback呼び出しを中心に数え、構文解析、コンパイル、直線的な大量コード、文字列サイズ、重いbuiltin、I/O待ち、プロセス全体の総heap量は包括的に制限しない。filesystem制御にもsymlinkのTOCTOU制約がある。

### ステップ予算

環境変数 `TSUMUGI_MAX_STEPS` でループ反復 + 関数呼び出しの合計回数に上限を設定する。超過するとランタイムエラー。

- デフォルト: 1,000,000（百万ステップ）
- REPLでは入力単位で予算をリセットする。1つの入力から呼ばれた関数・callback・importは同じ予算を共有する
- 例: `TSUMUGI_MAX_STEPS=5000000`

### コレクションサイズ上限

環境変数 `TSUMUGI_MAX_COLLECTION_SIZE` で、List/Dictの要素数とListを生成する操作の上限を設定する。超過すると `collection_limit` ランタイムエラーになる。

- デフォルト: 1,000,000要素
- 対象: List/Dictリテラル、`push`、辞書への新規キー追加、`range`、`split`、`read_lines`、`keys`/`values`、`map`/`filter`、`list_dir`、反復用List変換など
- 既存要素の置換や、入力より要素数が増えない操作は新たな要素を追加しない
- これは要素数の上限であり、文字列サイズやプロセス全体の総ヒープ量を制限するものではない
- 例: `TSUMUGI_MAX_COLLECTION_SIZE=100000`

### ファイルI/Oサンドボックス

環境変数 `TSUMUGI_SANDBOX` でファイル操作（read_file, write_file, append_file, import 等）の対象パスを制限する。

- カンマ区切りで複数パスを許可可能
- 許可パスのプレフィックスに合致しないアクセスはサンドボックス違反エラー
- 読み書き・metadataは、解決可能なfinal symlinkではtargetを認可対象とする。importは既存pathをcanonicalizeしてから認可する
- `remove` / `remove_dir` / `rename`は中間symlinkを解決したうえでfinal directory entryを認可対象とし、final symlinkのtargetは操作しない
- targetが未作成のdangling final symlinkを通じた`write_file` / `append_file`は、targetの場所を認可できない既知の制約がある（AUD-020）
- 検査と実I/Oの間のsymlink差し替えraceまでは防止しない
- 未設定時は制限なし（全パス許可）
- 例: `TSUMUGI_SANDBOX=/home/user/scripts,/tmp`

### 環境変数アクセス制御

環境変数 `TSUMUGI_ENV_ALLOW` で `env()` 関数が読み取れるキーを制限する。

- カンマ区切りでキー名を列挙
- 末尾 `*` でプレフィックスマッチ（例: `TSUMUGI_*`）
- 許可リスト外のキーは `null` を返す（エラーにはならない）
- 未設定時は制限なし（全キー読み取り可能）
- 例: `TSUMUGI_ENV_ALLOW="HOME,PATH,TSUMUGI_*"`

### コールフレーム深度制限

ユーザー関数呼び出しのネスト深度を128フレームに制限し、通常の関数再帰をプロセスクラッシュ前にランタイムエラー化する。

- 上限: 128フレーム
- 超過時: 「スタックオーバーフロー: 再帰が深すぎます」ランタイムエラー
- 設定変更: 不可（コンパイル時定数）

### 構文・AST深度制限

構文とASTのネスト深度を256に制限する。Parserはblock・括弧・単項演算・`elif`・連続二項演算・call/index・lambda・nested f-stringを含む生成経路で上限を検査する。さらに、公開AST APIからParserを迂回した入力はCompiler/Evaluator入口の非再帰preflightで検査する。

- 上限: rootの文を深度1として数える最長path 256ノード
- source超過時: 「式のネストが深すぎます」または「ネストが深すぎます」パースエラー
- Parserを迂回したASTの超過時: `overflow`ランタイムエラー。これはCompiler/Evaluator自身の走査を開始前に止める保証であり、借用元が任意深度で構築したcaller所有ASTのDropまでは管理しない
- 設定変更: 不可（コンパイル時定数）

### import chain深度制限

現在処理中の非循環import chainを128ファイルに制限する。root scriptは数えず、正規化済みの既訪問パスは深度を消費せずスキップする。

- 上限: 128ファイル
- 129段目: `import 失敗: ネストが深すぎます (上限: 128)`という`import`ランタイムエラー
- 対象: ツリーウォーク版の再帰実行とVM版の再帰inline compile
- 設定変更: 不可（コンパイル時定数）

### TSUMUGI_* 環境変数の保護

`env()` 関数は `TSUMUGI_` で始まるキーへのアクセスを常に `null` で返す。ランタイム制御用環境変数（`TSUMUGI_SANDBOX`, `TSUMUGI_MAX_STEPS` 等）の値がスクリプトに漏洩することを防ぐ。

prefix照合はOSの環境変数名規則に合わせる。WindowsではキーをUnicode uppercaseへ変換してから照合し、ASCIIの大小文字違いに加えてASCII名へ別名解決され得るUnicode case variantも保護する。その他のOSではcase-sensitiveに照合する。いずれの場合も、この保護は`TSUMUGI_ENV_ALLOW`の許可判定より先に適用する。
