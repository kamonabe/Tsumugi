# Tsumugi Language Guide for AI Code Assistants

This document describes the Tsumugi programming language syntax and semantics.
When generating or editing `.tsg` files, follow these rules strictly.

Tsumugi resembles Ruby and Python in style but is a distinct language with its own grammar.
Do NOT apply Ruby or Python conventions unless they align with the rules below.

---

## Quick Reference

| Aspect | Tsumugi | Ruby (differs) | Python (differs) |
|---|---|---|---|
| Block terminator | `end` | `end` (same) | indentation (no `end`) |
| Function keyword | `fn` | `def` | `def` |
| Variable declaration | `let x = val` | not required | not required |
| Null literal | `null` | `nil` | `None` |
| Boolean literals | `true` / `false` | `true` / `false` (same) | `True` / `False` |
| Logical operators | `and` / `or` / `not` | `&&` / `||` / `!` (also `and`/`or`) | `and` / `or` / `not` (same) |
| Comment | `# ...` | `# ...` (same) | `# ...` (same) |
| String interpolation | `f"...{expr}..."` | `"...#{expr}..."` | `f"...{expr}..."` (same) |
| Explicit return | required (`return expr`) | optional (last expr) | required (same) |
| Lambda syntax | `fn(x) expr end` | `-> (x) { expr }` / `lambda` | `lambda x: expr` |
| Loop else | not supported | not supported | supported |
| Class | not supported | supported | supported |
| Semicolons | not used | optional | not used (same) |
| Type system | dynamic, no annotations | dynamic | dynamic (annotations exist) |
| Import | `import "path.tsg"` | `require` / `require_relative` | `import` / `from ... import` |

---

## Syntax Rules

### Comments

```tsg
# Single-line comment only. No block comments.
```

### Variable Declaration and Reassignment

```tsg
# Declaration requires `let`
let x = 10
let name = "tsumugi"

# Reassignment: no `let`, variable must already be declared
x = 20
name = "updated"
```

**WRONG (Ruby/Python style):**
```
x = 10        # ERROR: undeclared variable assignment
var x = 10    # ERROR: no `var` keyword
```

### Functions

```tsg
fn add(a, b)
    return a + b
end
```

- Keyword is `fn`, NOT `def`
- Body ends with `end`
- Return is explicit: `return expr`
- No implicit return of last expression (unlike Ruby)
- No type annotations on parameters or return
- User-defined calls run in this order: step/depth check, callee evaluation, callable/arity validation, arguments left-to-right, then the function body
- If the callee is not callable or the arity is wrong, arguments and the function body are not evaluated
- If callee evaluation or an argument fails, later arguments and the function body are not evaluated

**WRONG:**
```
def add(a, b)       # ERROR: `def` is not a keyword
    return a + b
end

fn add(a, b):       # ERROR: no colon after parameters
    return a + b
end

fn add(a, b)
    a + b           # This does NOT return; it's just a discarded expression
end
```

### Anonymous Functions (Lambda)

```tsg
# Multi-line
let double = fn(x)
    return x * 2
end

# Single-line (implicit return)
let double = fn(x) x * 2 end

# Passed as argument
map(list, fn(x) x * 2 end)
```

- Always wrapped with `fn(params) ... end`
- Single-line form: the expression after params is implicitly returned
- No `->`, no `lambda` keyword, no `do...end` blocks

**WRONG:**
```
let f = lambda x: x * 2        # ERROR: no `lambda` keyword
let f = -> (x) { x * 2 }       # ERROR: no arrow syntax
let f = |x| x * 2              # ERROR: no pipe syntax
list.map { |x| x * 2 }         # ERROR: no block syntax
```

### Control Flow

```tsg
if condition
    body
elif other_condition
    body
else
    body
end
```

- `elif`, NOT `elsif` (Ruby) and NOT `else if` as two words
- No parentheses around condition (they are allowed but not idiomatic)
- No colon after condition
- Terminated by `end`

**WRONG:**
```
if condition:       # ERROR: no colon
    body
end

if (condition) {    # ERROR: no braces
    body
}

if condition
    body
elsif ...           # ERROR: `elsif` is not valid, use `elif`
end
```

### Loops

```tsg
while condition
    body
end

for item in collection
    body
end
```

- `for ... in` iterates over lists, dicts (by key), and strings (by character)
- No `do` keyword after condition
- No range operator like `1..10`; use `range(start, end)` built-in instead
- `break` and `continue` are supported

**WRONG:**
```
for i in 1..10          # ERROR: no range operator
for item in collection do   # ERROR: no `do`
while condition do          # ERROR: no `do`
for i in range(10):         # ERROR: no colon
```

### Scoping

- Each selected `if` / `elif` / `else` body has its own block scope
- Each `while` / `for` iteration has its own block scope
- A `for` loop variable is rebound to a fresh variable cell at the start of every iteration. Closures from different iterations retain separate cells; closures and assignments within the same iteration share that iteration's cell
- `try` and `catch` have separate block scopes
- The catch variable exists only inside its `catch` block
- Functions create their own lexical scope
- Names are checked when the expression or assignment is executed, not while unreachable code is compiled. Undefined names in dead branches, short-circuited operands, uncalled functions, or code after `return` do not fail
- A top-level `let` or function becomes globally visible when its declaration executes; globals are not hoisted before that point
- Functions capture lexical bindings visible at definition time. A name unresolved at definition uses the live global scope when executed, so a function may reference a later top-level declaration if it is called after that declaration; top-level mutual recursion is valid after both definitions run
- A block-local declared after a function definition is not a forward global and is not made visible to that function
- Missing variables/functions produce catchable `name` runtime errors; a missing callee does not evaluate its arguments
- `let` declares or shadows a name in the current scope; assignment without `let` updates the nearest existing binding
- A closure may keep a captured block-local cell alive after block exit, but the name is not directly visible outside
- Scope cleanup happens on normal completion, errors, `return`, `break`, and `continue`
- For normal completion or an error caught within the same execution, scope cleanup itself does not roll back assignment to outer variables, collection mutation, or external I/O
- Commit/rollback after an unhandled REPL submission is not yet unified between engines and remains tracked by AUD-024

```tsg
let total = 0
for i in [1, 2, 3]
    let temp = i * 2      # local to this iteration
    total = total + temp  # assignment updates the outer variable
end
print(total)              # 12

let result = null
if true
    let temporary = "done"
    result = temporary    # export through a predeclared outer binding
end
print(result)             # done
# print(temporary)        # ERROR: not defined outside the if block
```

**WRONG:**
```
let count = 3
while count > 0
    let count = count - 1  # ERROR: shadows count; the outer value never changes
end

if true
    let result = "local"
end
print(result)              # ERROR: result is block-local
```

### Data Types

| Type | Examples |
|---|---|
| Integer | `0`, `42`, `-3` |
| Float | `3.14`, `0.5` |
| String | `"hello"`, `"line\n"` |
| Boolean | `true`, `false` |
| Null | `null` |
| List | `[1, "two", true]` |
| Dict | `{"key": value}` |

- No symbols (`:name` does not exist)
- No tuples
- Dict keys must be strings
- `null`, not `nil`, not `None`
- `true`/`false`, not `True`/`False`

### String Interpolation (f-string)

```tsg
let name = "world"
print(f"hello, {name}")
print(f"result: {1 + 2}")
```

- Prefix `f` before the string literal
- Expressions inside `{...}`
- Escape literal braces: `{{` and `}}`
- NOT `#{}` (Ruby-style interpolation)

**WRONG:**
```
print("hello, #{name}")     # ERROR: this is Ruby syntax, not tsumugi
print(f"hello, #{name}")    # ERROR: use {name} not #{name}
```

### Error Handling

```tsg
try
    risky_operation()
catch e
    print("error: " + e)
end
```

- `try ... catch variable ... end`
- NOT `begin/rescue/end` (Ruby)
- NOT `try/except` (Python)
- No `finally` or `ensure` block
- The catch variable is bound to a structured Error value, not a plain string
- `try` and `catch` have separate scopes; declarations made with `let` in `try` are not visible in `catch`
- The catch variable and declarations made in `catch` disappear when that block ends
- To pass a value out of either block, declare it outside and update it with assignment
- Access fields via index syntax: `e["type"]`, `e["message"]`, `e["line"]`
- It also stringifies automatically when concatenated with `+` or interpolated in an f-string
- `e["type"]` values (snake_case identifiers):
  `zero_division`, `type`, `index`, `name`, `limit`, `overflow`,
  `sandbox`, `import`, `argument`, `int_overflow`, `control_flow`,
  `collection_limit`, `conversion`, `builtin_type`, `iteration`,
  `internal`, `runtime`

**WRONG:**
```
begin                       # ERROR: no `begin` keyword
    risky()
rescue e                    # ERROR: no `rescue`
end

try:                        # ERROR: no colon
    risky()
except Exception as e:      # ERROR: no `except`, no type, no `as`
    pass
```

### Import

```tsg
import "path/to/module.tsg"
```

- Import statements are valid only at the program top level; using one inside any block is a parse error
- All definitions from the imported file are injected into the current scope (flat, no namespace)
- Path is relative to the current script file
- No `require`, no `from ... import`, no selective imports
- Circular imports are silently skipped (not an error)

**WRONG:**
```
require "module"                    # ERROR: no `require`
from module import func             # ERROR: no `from` import
import module                       # ERROR: must be a string path
import "module" as m                # ERROR: no `as` aliasing
if true
    import "module.tsg"             # ERROR: import must be top-level
end
```

### Operators

- Arithmetic: `+`, `-`, `*`, `/`, `%`
- Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Logical: `and`, `or`, `not` (English words, NOT symbols)
- String concatenation: `+`
- No `**` power operator; use a custom function or loop
- No `//` integer division operator; `/` on integers gives integer division
- No `+=`, `-=`, etc. compound assignment operators

**`and`/`or` semantics (Python/JS-style):**
- Short-circuit evaluation: right side is not evaluated if result is determined by left side
- `and` returns the left value if falsy, otherwise the right value
- `or` returns the left value if truthy, otherwise the right value
- Useful idiom: `let x = value or default`

```tsg
print(true and 5)       # 5 (left truthy → returns right)
print(false and 5)      # false (left falsy → returns left)
print(null or "default") # default (left falsy → returns right)
print("hi" or "bye")   # hi (left truthy → returns left)
```

**WRONG:**
```
x += 1              # ERROR: no compound assignment
2 ** 10             # ERROR: no power operator
x && y              # ERROR: use `and`
x || y              # ERROR: use `or`
!flag               # ERROR: use `not flag`
```

### Built-in Functions (Calling Convention)

Built-in functions are called as free functions, NOT as methods.

```tsg
# Correct
len(my_list)
push(my_list, item)
keys(my_dict)
split(my_string, ",")
upper(my_string)
map(my_list, fn(x) x * 2 end)

# WRONG - no method syntax
my_list.length          # ERROR
my_list.append(item)    # ERROR
my_dict.keys()          # ERROR
my_string.split(",")    # ERROR
my_string.upper()       # ERROR
my_list.map { ... }     # ERROR
```

### Index Access and Assignment

```tsg
let xs = [10, 20, 30]
print(xs[0])        # 10
print(xs[-1])       # 30 (negative index from end)

let d = {"key": "val"}
print(d["key"])     # val

# Assignment
xs[0] = 99
d["new_key"] = 123
```

- Negative index supported (like Python)
- Dict missing key access returns `null` (no KeyError)
- No nested index assignment (`xs[0][1] = val` is NOT supported)
- No slice syntax (`xs[1:3]`); use `slice(xs, 1, 3)` built-in

**WRONG:**
```
xs[1:3]             # ERROR: no slice syntax
d.key               # ERROR: no dot access for dicts
```

---

## Key Behavioral Differences from Ruby

| Topic | Ruby | Tsumugi |
|---|---|---|
| Last expression return | Implicit return of last expression | Must use explicit `return` |
| Blocks | `do...end` or `{...}` with `\|params\|` | `fn(params) ... end` passed as argument |
| Method calls | `obj.method(args)` | Free functions only: `func(obj, args)` |
| Symbols | `:name` | Not supported |
| Nil check | `.nil?` | `== null` |
| Array methods | `arr.push(x)`, `arr.map {...}` | `push(arr, x)`, `map(arr, fn(x) ... end)` |
| String interpolation | `"#{expr}"` | `f"{expr}"` |
| Exception handling | `begin/rescue/ensure/end` | `try/catch/end` |
| require | `require "lib"` | `import "path.tsg"` |
| elsif | `elsif` | `elif` |
| Range | `1..10`, `(1...10)` | `range(1, 10)` |
| Class/module | `class Foo ... end` | Not supported |

## Key Behavioral Differences from Python

| Topic | Python | Tsumugi |
|---|---|---|
| Block structure | Indentation-based | `end` keyword terminates blocks |
| Function keyword | `def` | `fn` |
| None / null | `None` | `null` |
| Booleans | `True` / `False` | `true` / `false` |
| elif/else if | `elif` (same) | `elif` (same) |
| Lambda | `lambda x: expr` | `fn(x) expr end` |
| List comprehension | `[x*2 for x in xs]` | `map(xs, fn(x) x*2 end)` |
| Dictionary access | `d["key"]` raises KeyError | `d["key"]` returns `null` |
| try/except | `try/except/finally` | `try/catch/end` (no finally) |
| import system | `import mod` / `from mod import x` | `import "path.tsg"` (flat injection) |
| Type hints | `def f(x: int) -> int:` | Not supported |
| Decorators | `@decorator` | Not supported |
| Compound assignment | `x += 1` | Not supported; use `x = x + 1` |
| Multiple return | `return a, b` (tuple) | Not supported; return a list instead |
| f-string | `f"{expr}"` (same syntax) | `f"{expr}"` (same syntax) |
| Slice | `xs[1:3]` | `slice(xs, 1, 3)` |
| with statement | `with open(f) as fh:` | Not supported |
| pass | `pass` (no-op) | Not needed; empty blocks are valid |

---

## Common Patterns

### Iterating and Filtering

```tsg
let nums = [1, 2, 3, 4, 5]

# Functional style (preferred for simple transforms)
let doubled = map(nums, fn(x) x * 2 end)
let evens = filter(nums, fn(x) x % 2 == 0 end)

# Imperative style
let result = []
for n in nums
    if n > 3
        push(result, n)
    end
end
```

### Building Strings

```tsg
# Concatenation
let msg = "hello" + " " + "world"

# f-string interpolation
let name = "tsumugi"
let version = 1
let msg = f"{name} v{version}"

# Joining a list
let parts = ["a", "b", "c"]
let joined = join(parts, ", ")
```

### Closures

```tsg
fn make_counter(start)
    let count = start
    fn increment()
        count = count + 1
        return count
    end
    return increment
end

let c = make_counter(0)
print(c())  # 1
print(c())  # 2
print(c())  # 3
```

> Tsumugi uses reference capture for closures. Closures share variable cells with their
> defining scope via `Rc<RefCell<Value>>`. Mutations inside a closure are visible to
> subsequent calls (counter pattern works).
>
> Function values are never equal to each other (`f == f` is always `false`).
> Do not use `==` or `contains()` to compare functions.

### Error Handling Pattern

```tsg
fn safe_divide(a, b)
    if b == 0
        return null
    end
    return a / b
end

# Or with try/catch
try
    let result = 10 / 0
catch e
    # e stringifies to the message when interpolated
    print(f"caught: {e}")
    # structured access via index
    print(e["type"])       # e.g. "zero_division"
    print(e["message"])    # e.g. "division by zero"
    print(e["line"])       # line number (integer)
end
```

---

## Anti-Patterns (DO NOT Generate)

### 1. Method chaining
```
# WRONG — Tsumugi has no methods
"hello".upper().split("")
[1,2,3].map(fn(x) x end).filter(fn(x) x > 1 end)

# Correct
let s = upper("hello")
let parts = split(s, "")
let mapped = map([1,2,3], fn(x) x end)
let filtered = filter(mapped, fn(x) x > 1 end)
```

### 2. Using `def` instead of `fn`
```
# WRONG
def greet(name)
    return "hi " + name
end

# Correct
fn greet(name)
    return "hi " + name
end
```

### 3. Using Ruby-style string interpolation
```
# WRONG
let msg = "hello #{name}"

# Correct
let msg = f"hello {name}"
```

### 4. Implicit return
```
# WRONG — This does NOT return the value
fn double(x)
    x * 2
end

# Correct
fn double(x)
    return x * 2
end
```

### 5. Using `elsif` instead of `elif`
```
# WRONG
if x > 0
    print("pos")
elsif x < 0
    print("neg")
end

# Correct
if x > 0
    print("pos")
elif x < 0
    print("neg")
end
```

### 6. Compound assignment
```
# WRONG
count += 1
total -= item

# Correct
count = count + 1
total = total - item
```

### 7. Using `nil` or `None`
```
# WRONG
let x = nil
let y = None

# Correct
let x = null
```

### 8. Undeclared variable assignment
```
# WRONG — no `let` on first use
x = 42

# Correct
let x = 42
```

### 9. Slice syntax
```
# WRONG
let sub = list[1:3]

# Correct
let sub = slice(list, 1, 3)
```

### 10. Python/Ruby exception syntax
```
# WRONG
try:
    dangerous()
except ValueError as e:
    handle(e)

begin
    dangerous()
rescue => e
    handle(e)
end

# Correct
try
    dangerous()
catch e
    print(e)
end
```

---

## File Extension

Tsumugi source files use the `.tsg` extension.

---

## Grammar (Formal)

```
program        = top_level_stmt*
top_level_stmt = import_stmt | stmt
stmt           = let_stmt | assign_stmt | index_assign | return_stmt
               | if_stmt | while_stmt | for_stmt | break_stmt | continue_stmt
               | fn_def | try_catch_stmt | expr_stmt
let_stmt       = "let" IDENT "=" expr NEWLINE
assign_stmt    = IDENT "=" expr NEWLINE
index_assign   = postfix "[" expr "]" "=" expr NEWLINE
return_stmt    = "return" expr NEWLINE
if_stmt        = "if" expr NEWLINE block ("elif" expr NEWLINE block)* ("else" NEWLINE block)? "end" NEWLINE
while_stmt     = "while" expr NEWLINE block "end" NEWLINE
for_stmt       = "for" IDENT "in" expr NEWLINE block "end" NEWLINE
break_stmt     = "break" NEWLINE
continue_stmt  = "continue" NEWLINE
import_stmt    = "import" STRING NEWLINE
try_catch_stmt = "try" NEWLINE block "catch" IDENT NEWLINE block "end" NEWLINE
fn_def         = "fn" IDENT "(" params? ")" NEWLINE block "end" NEWLINE
expr_stmt      = expr NEWLINE
block          = stmt*
params         = IDENT ("," IDENT)*
expr           = or_expr
or_expr        = and_expr ("or" and_expr)*
and_expr       = cmp_expr ("and" cmp_expr)*
cmp_expr       = add_expr (("==" | "!=" | "<" | ">" | "<=" | ">=") add_expr)*
add_expr       = mul_expr (("+" | "-") mul_expr)*
mul_expr       = unary_expr (("*" | "/" | "%") unary_expr)*
unary_expr     = ("not" | "-") unary_expr | postfix
postfix        = primary ( "(" args? ")" | "[" expr "]" )*
primary        = INT | FLOAT | STRING | FSTRING | "true" | "false" | "null"
               | IDENT | "(" expr ")" | list_literal | dict_literal | lambda
list_literal   = "[" (expr ("," expr)* ","?)? "]"
dict_literal   = "{" (STRING ":" expr ("," STRING ":" expr)* ","?)? "}"
lambda         = "fn" "(" params? ")" NEWLINE block "end"
               | "fn" "(" params? ")" expr "end"
args           = expr ("," expr)*
```

---

## Truthiness

Falsy values (everything else is truthy):
- `false`
- `null`
- `0` (integer zero)
- `0.0` (float zero)
- `""` (empty string)
- `[]` (empty list)
- `{}` (empty dict)

---

## Summary of Reserved Keywords

```
let fn return if elif else end while for in
break continue import try catch true false null and or not print
```

These cannot be used as variable or function names. `print` is tokenized specially and cannot be shadowed or captured as a first-class function value; call it directly as `print(...)`.

---

## Implementation Conformance Notes

This guide and `docs/language-spec.md` describe the normative language. Both execution engines are expected to follow that specification, but the current alpha implementation has known deviations tracked in `docs/roadmap.md`.

When generating portable `.tsg` code:

- Always close multi-line lambdas with `end`. The current parser may accidentally accept EOF without it; that input is invalid.
- Use string literals or expressions that evaluate to strings for Dict keys. The parser may accept another expression form, but runtime semantics require a String key.
- Do not depend on equality for List, Dict, Function, or Error values, mixed Int/Float comparison, local named-function recursion, same-scope redeclaration cell identity, captured collection index assignment, or import side-effect timing until the engine-parity issues are resolved.
- Treat `TSUMUGI_SANDBOX` and resource limits as defense-in-depth, not as isolation for untrusted code.
