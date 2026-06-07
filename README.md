# spindle

`spindle` は、macOS のローカル自動化をイベント・アクション・拡張でつなぐ小さなハーネスです。

中心にあるのは、特定のアプリ連携を直接持たないカーネルです。カーネルはイベントを保存し、拡張の登録情報を検証し、イベントからアクションへのルートを実行します。
`AeroSpace`、`SketchyBar`、Raycast、エージェントのフックなど、実際のワークフローは拡張として外に置きます。

## 何をするものか

`spindle` は次のようなローカル連携を、個別のブリッジバイナリを増やさずに組み合わせるための土台です。

```text
イベントを受け取る
  -> append-only JSONL ログに保存する
  -> インストール済みルートを探す
  -> 拡張アクションを呼ぶ
  -> アクションが出したイベントをさらに dispatch する
```

たとえば公式拡張を組み合わせると、次の流れを構成できます。

```text
aerospace.workspace.changed
  -> aerospace.workspace.snapshot
  -> workspace-indicator.workspaces.render
  -> sketchybar.message.requested
  -> sketchybar.message.send

sketchybar.workspace.clicked
  -> aerospace.workspace.focus
```

この構成では、`AeroSpace` 拡張は AeroSpace IPC だけを扱い、`SketchyBar` 拡張は SketchyBar IPC だけを扱います。
ワークスペース表示の色・ラベル・キャッシュキーなどの投影ロジックは `workspace-indicator` 拡張が持ちます。

## カーネルが持つ責務

- UUID v7 形式の ID を持つ append-only JSONL イベントログ
- Unix domain socket で受ける JSONL リクエスト
- `emit` によるイベント追加とルート dispatch
- `invoke` による直接アクション実行と `action.requested` ログ記録
- 拡張 manifest の検証
- stdio JSONL 拡張ホストの登録・起動・再利用
- 拡張が宣言する event/action/capability surface の所有権チェック
- capability policy による direct invoke / route grant の制御
- アクション出力イベントの再帰 dispatch（深さ上限あり）

カーネルは、フィルタ、テンプレート、リトライ、分岐、ワークフロー固有状態を持ちません。それらは拡張側の責務です。

## ワークスペース構成

- `src/` — `spindle` カーネル、CLI、socket server
- `crates/spindle-extension-sdk/` — stdio JSONL 拡張ホスト用の型付き SDK
- `extensions/aerospace/` — AeroSpace IPC と workspace/mode/layout snapshot、workspace focus
- `extensions/sketchybar/` — SketchyBar Mach IPC と generic message send
- `extensions/workspace-indicator/` — AeroSpace 状態を SketchyBar message request に変換する workflow 拡張
- `examples/` — manifest 例と小さな実験用コード

## セットアップ

Rust は `rust-toolchain.toml` で nightly に固定されています。リポジトリを clone したあと、rustup が自動で toolchain を入れます。

任意で [Task](https://taskfile.dev/) と [Lefthook](https://github.com/evilmartians/lefthook) を使えます。

```bash
task              # 利用可能な task を表示
task build        # cargo build --workspace --locked
task test         # 全テスト
task lint         # clippy -D warnings
task fmt          # format
task check        # fmt, lint, test, doc, build
```

Cargo だけでも実行できます。

```bash
cargo build --workspace --locked
cargo test --workspace --all-targets --all-features --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

release ビルドで公式拡張の entrypoint が `target/release/` に作られます。

```bash
cargo build --workspace --release --locked
```

## 基本的な使い方

実験中は明示的な state directory を使うと安全です。

```bash
export SPINDLE_STATE_DIR=/tmp/spindle
```

イベントを追加します。

```bash
cargo run -- emit \
  --type agent.status.changed \
  --source pi \
  --data '{"state":"working","message":"cargo test"}'
```

イベントを読むには `query events` を使います。

```bash
cargo run -- query events --type agent.status.changed
cargo run -- query events --source pi --limit 5
```

daemon を起動すると、同じ request contract を Unix socket 経由で使えます。

```bash
cargo run -- daemon
```

別の shell から JSONL request を送ります。

```bash
cargo run -- send \
  --request '{"command":"emit","type":"agent.status.changed","source":"pi","data":{"state":"testing"}}'
```

socket path は既定で `<state-dir>/spindle.sock` です。`--socket` で明示できます。

## 拡張をインストールする

公式拡張を使う場合は、先に release build します。

```bash
cargo build --workspace --release --locked
cargo run -- install aerospace
cargo run -- install sketchybar
cargo run -- install workspace-indicator
cargo run -- extension list
```

`install aerospace` のように名前だけを渡すと、`extensions/<name>/extension.json` を読みます。
manifest path や manifest を含むディレクトリも指定できます。

```bash
cargo run -- install extensions/aerospace/extension.json
cargo run -- install extensions/aerospace
```

manifest を検証するだけなら、拡張ホストは起動されません。

```bash
cargo run -- extension validate extensions/aerospace/extension.json
```

`install` / `extension register` は trusted operation です。
`stdio-jsonl` 拡張では entrypoint を起動し、`register` request で拡張コードから surface を受け取ります。

## capability policy

アクションが capability を要求する場合、direct invoke や route はその capability を grant する必要があります。
さらに、その grant は state directory の `capabilities.json` で許可されている必要があります。

```json
{
  "direct": {
    "raycast": ["aerospace.window.control"]
  },
  "routes": {
    "workspace-indicator": [
      "aerospace.state.read",
      "aerospace.window.control",
      "sketchybar.ui.write"
    ]
  }
}
```

直接アクションを呼ぶ例です。

```bash
cargo run -- invoke \
  --action aerospace.workspace.focus \
  --source raycast \
  --capability aerospace.window.control \
  --args '{"name":"dev"}'
```

`source` と拡張 ID はローカルな論理名です。OS の sandbox やプロセス認証ではありません。`spindle` の境界は、ローカル state に置いた grant policy です。

## manifest と登録 surface

最小の `stdio-jsonl` manifest は、拡張 ID、version、entrypoint、runtime だけを持ちます。

```json
{
  "id": "sketchybar",
  "version": "0.1.0",
  "entrypoint": "../../target/release/spindle-sketchybar",
  "runtime": "stdio-jsonl"
}
```

実際の event/action/capability は、manifest に静的に書くこともできますが、公式拡張では SDK を使って拡張コードから登録します。

```rust
ExtensionRegistration::new()
    .emit("sketchybar.workspace.clicked")
    .capability("sketchybar.ui.write")
    .action(
        "sketchybar.message.send",
        RegistrationAction::new().capability("sketchybar.ui.write"),
    )
```

ルートは event から action への小さな接続です。

```json
{
  "event": "sketchybar.workspace.clicked",
  "action": "aerospace.workspace.focus",
  "capabilities": ["aerospace.window.control"],
  "args": {}
}
```

dispatch 時は、イベント payload と route の static `args` が object として merge されます。同じ key がある場合は route の `args` が優先されます。

## stdio JSONL 拡張ホスト

`spindle-extension-sdk` は、カーネルと拡張ホストの間で使う型付き contract を提供します。

拡張ホストは stdin/stdout で次の request を受けます。

- `register` — `ExtensionRegistration` を返す
- `invoke` — `ActionInvocation` を受けて `ActionOutput` を返す
- `shutdown` — ホストを終了する

アクションは `ActionOutput` でイベントを返せます。返されたイベントはイベントログに保存され、さらに一致する route が dispatch されます。

`ActionContext::extension()` には daemon が見ている installed surface が入ります。
workflow 拡張はこれを使って、必要な provider event や action があるかを実行時に確認できます。

## 公式拡張

### `aerospace`

AeroSpace の Unix socket IPC を扱います。

主な surface:

- emits: `aerospace.workspace.changed`, `aerospace.focus.changed`, `aerospace.monitor.changed`,
  `aerospace.mode.changed`, `aerospace.layout.changed`
- actions: `aerospace.workspace.focus`, `aerospace.workspace.snapshot`,
  `aerospace.mode.snapshot`, `aerospace.layout.snapshot`
- capabilities: `aerospace.state.read`, `aerospace.window.control`

### `sketchybar`

SketchyBar の Mach IPC を扱います。SketchyBar CLI を毎回 spawn せず、NUL 区切りの message payload を直接送ります。

主な surface:

- emits: `sketchybar.workspace.clicked`
- actions: `sketchybar.message.send`
- capabilities: `sketchybar.ui.write`

`sketchybar.message.send` は `cache_key` / `cache_value` を受けると、同じ内容の再送を抑制します。

### `workspace-indicator`

Provider I/O は行わない workflow 拡張です。
AeroSpace snapshot を SketchyBar message request に変換し、`sketchybar.message.requested` を emit します。

既定の workspace list は `1,2,3,4,5,6,7,8,9,10` です。

## 状態ファイル

state directory には主に次のファイルが置かれます。

- `events.jsonl` — append-only event log
- `extensions.json` — install/register 済み拡張
- `capabilities.json` — direct / route capability grant policy
- `spindle.sock` — daemon の Unix socket

既定の state directory は `$SPINDLE_STATE_DIR` があればその値、なければ `$HOME/.local/state/spindle` です。

## ライセンス

MIT
