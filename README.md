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
aerospace.workspace.changed / aerospace.monitor.changed
  -> workspace-indicator.workspaces.schedule
  -> extension-owned latest-wins scheduler
  -> aerospace.workspace.snapshot
  -> workspace-indicator.workspaces.render
  -> workspace-indicator.sketchybar.message.requested
  -> sketchybar.message.send

sketchybar.workspace.clicked
  -> aerospace.workspace.focus
```

この構成では、`AeroSpace` 拡張は AeroSpace IPC だけを扱い、`SketchyBar` 拡張は SketchyBar IPC だけを扱います。
ワークスペース表示の色・ラベル・キャッシュキーなどの投影ロジックと、rapid switching 時に中間状態を捨てる latest-wins debounce 方針は `workspace-indicator` 拡張の scheduler が持ちます。

## カーネルが持つ責務

- UUID v7 形式の ID を持つ append-only JSONL イベントログ
- Unix domain socket で受ける JSONL リクエスト
- `emit` によるイベント追加とルート dispatch
- `invoke` による直接アクション実行と `action.requested` ログ記録
- 拡張 manifest の検証
- stdio JSONL 拡張ホストの登録・起動・再利用
- 拡張が宣言する event/action/capability surface の所有権チェック
- capability policy による direct invoke / route grant の制御
- capability-scoped continuation handle による extension の deferred work
- アクション出力イベントの再帰 dispatch（深さ上限あり）

## カーネルに含めないもの（ネガティブスペース）

以下の機能は意図的にコアから除外しています。
新たな機能追加の際は、この表を判断基準として「コアか拡張か」を決定します。

| 除外する機能 | コアに含めない理由 | 拡張パス |
|--------------|-------------------|----------|
| イベントフィルタリング | フィルタ条件はワークフローごとに異なり、コアが関与すると変更のたびに再ビルドが必要になる。ルートの `source` マッチで十分 | ルート定義の `source` フィールドで実現 |
| テンプレートエンジン | メッセージ形式や表示ロジックはドメイン固有。コアが特定のテンプレート言語を持つと、拡張の表現力を制限する | 各拡張が自身のレンダリングロジックを持つ（例: `workspace-indicator` が SketchyBar メッセージを生成） |
| リトライ・再実行 | リトライ戦略（回数・間隔・バックオフ）はアクションの性質に依存し、コアの固定ロジックでは不十分 | 拡張側で実装するか、将来の命令面設定（リトライポリシー）で宣言的に指定 |
| 条件分岐（if/else） | 分岐ロジックをコアが持つと、拡張が自由にワークフローを構成できなくなる。分岐はワークフロー拡張の責務 | `workspace-indicator` のような workflow 拡張が action output で分岐を実現 |
| ワークフロー固有状態 | 進捗カウンタや中間状態をコアが保持すると、拡張間の暗黙的結合が生まれ、テストと再利用が困難になる | 各拡張が自身の状態を持つか、イベントログから必要な情報をクエリ |
| スケジューリング・タイマー | 定期実行の要件は外部ツール（launchd, cron）の責務。コアに取り込むとプロセス管理が複雑化する | 外部トリガーから `emit` または `invoke` を発行 |
| 通知・アラート | 通知先（OS通知、Webhook、メッセージング）は環境ごとに異なり、コアが全経路を持つと肥大化する | 通知用拡張を追加するか、既存拡張の action output を通知先にルート |
| イベント永続化ポリシー（保持期間・ローテーション） | 保持ポリシーは運用要件であり、コアの責務ではない | 外部ツールによるログローテーション、またはログ読み取り拡張 |
| サードパーティプロトコル（AeroSpace IPC, SketchyBar Mach IPC） | 特定アプリケーションのプロトコル実装をコアに含めると、未使用時も依存が残り、新規プロトコル追加のたびにコア変更が必要になる | `extensions/aerospace/`、`extensions/sketchybar/` が各プロトコルを実装 |
| プラグインの動的ロード（dlopen） | プロセス内動的ロードは安全性の境界が曖昧で、クラッシュがコア全体に波及する | stdio JSONL によるプロセス分離（拡張ごとに独立した子プロセス） |

### 判断基準

機能をコアに追加するのは、以下の**すべて**を満たす場合のみです:

1. **普遍性** — 拡張を一切インストールしなくても、すべてのセッションで必要になる
2. **安全性** — 第三者（拡張作者）に委ねると危険または不正な動作が避けられない
3. **安定性** — インターフェースがユーザーやチームごとに変化せず、長期にわたって固定できる

上記のいずれかを満たさない場合は、拡張面として設計します。

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

stdio dispatch benchmark は workspace-indicator の実行ファイルを明示する必要があります。

```bash
SPINDLE_WORKSPACE_INDICATOR_BIN=target/release/spindle-workspace-indicator \
  cargo run --example perf --release
```

`SPINDLE_WORKSPACE_INDICATOR_BIN` is required for stdio dispatch benchmark. Build `spindle-workspace-indicator` first and pass its path via `SPINDLE_WORKSPACE_INDICATOR_BIN`.

## 基本的な使い方

実験中は明示的な state directory を使うと安全です。

```bash
export SPINDLE_STATE_DIR=/tmp/spindle
mkdir -p "$SPINDLE_STATE_DIR"
chmod 700 "$SPINDLE_STATE_DIR"

cat > "$SPINDLE_STATE_DIR/capabilities.json" <<'JSON'
{
  "emits": {
    "pi": ["agent.status.changed"]
  },
  "direct": {},
  "routes": {}
}
JSON
chmod 600 "$SPINDLE_STATE_DIR/capabilities.json"
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

公式拡張を使う場合は、先に state directory と capability policy を作ります。

```bash
export SPINDLE_STATE_DIR="${SPINDLE_STATE_DIR:-$HOME/.local/state/spindle}"
mkdir -p "$SPINDLE_STATE_DIR"
chmod 700 "$SPINDLE_STATE_DIR"

cat > "$SPINDLE_STATE_DIR/capabilities.json" <<'JSON'
{
  "emits": {
    "aerospace": [
      "aerospace.workspace.changed",
      "aerospace.focus.changed",
      "aerospace.monitor.changed",
      "aerospace.mode.changed",
      "aerospace.layout.changed"
    ],
    "sketchybar": [
      "sketchybar.workspace.clicked"
    ],
    "pi": [
      "agent.status.changed"
    ]
  },
  "direct": {
    "raycast": [
      "aerospace.window.control"
    ]
  },
  "routes": {
    "workspace-indicator": [
      {
        "source": "aerospace",
        "event": "aerospace.workspace.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.monitor.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.mode.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.focus.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.layout.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "workspace-indicator",
        "event": "workspace-indicator.sketchybar.message.requested",
        "capabilities": ["sketchybar.ui.write"]
      },
      {
        "source": "sketchybar",
        "event": "sketchybar.workspace.clicked",
        "capabilities": ["aerospace.window.control"]
      }
    ]
  }
}
JSON
chmod 600 "$SPINDLE_STATE_DIR/capabilities.json"

cargo build --workspace --release --locked
cargo run -- install --trust-runtime aerospace
cargo run -- install --trust-runtime sketchybar
cargo run -- install --trust-runtime workspace-indicator
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

`extension validate` は static manifest だけを読み、entrypoint は起動しません。
`install` / `extension register` は既定では static manifest surface だけを登録します。
`stdio-jsonl` 拡張の entrypoint を起動して `register` request から dynamic surface を受け取る場合は、`--trust-runtime` を明示します。
`--trust-runtime` は dynamic surface discovery のために entrypoint を実行し、その時点の entrypoint path / SHA-256 を registry に記録します。
registry に書かず dynamic surface だけを見るには `extension surface --trust-runtime <manifest>` を使います。

## capability policy

アクションが capability を要求する場合、direct invoke や route はその capability を grant する必要があります。
さらに、その grant は state directory の `capabilities.json` で許可されている必要があります。
route capability は dispatch 時だけでなく、extension の install / register 時にも検査されます。
route grant policy は route owner extension id ごとに、許可する event `source` / event kind / capabilities を指定します。
たとえば `workspace-indicator` は aerospace provider event への route で `aerospace.state.read` を grant し、自身が emit する `workspace-indicator.sketchybar.message.requested` への route で `sketchybar.ui.write` を grant し、SketchyBar click event への route で `aerospace.window.control` を grant するため、install 前に `capabilities.json` の `routes.workspace-indicator` を設定してください。

```json
{
  "emits": {
    "aerospace": [
      "aerospace.workspace.changed",
      "aerospace.focus.changed",
      "aerospace.monitor.changed",
      "aerospace.mode.changed",
      "aerospace.layout.changed"
    ],
    "pi": ["agent.status.changed"],
    "sketchybar": ["sketchybar.workspace.clicked"]
  },
  "direct": {
    "raycast": ["aerospace.window.control"]
  },
  "routes": {
    "workspace-indicator": [
      {
        "source": "aerospace",
        "event": "aerospace.workspace.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.monitor.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.mode.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.focus.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "aerospace",
        "event": "aerospace.layout.changed",
        "capabilities": ["aerospace.state.read"]
      },
      {
        "source": "workspace-indicator",
        "event": "workspace-indicator.sketchybar.message.requested",
        "capabilities": ["sketchybar.ui.write"]
      },
      {
        "source": "sketchybar",
        "event": "sketchybar.workspace.clicked",
        "capabilities": ["aerospace.window.control"]
      }
    ]
  }
}
```

`capabilities.json` は local grant policy なので、手動で作る場合も `chmod 600 "$SPINDLE_STATE_DIR/capabilities.json"` で private file として扱ってください。

直接アクションを呼ぶ例です。

```bash
cargo run -- invoke \
  --action aerospace.workspace.focus \
  --source raycast \
  --capability aerospace.window.control \
  --args '{"name":"dev"}'
```

`emit` も dispatch の入口なので、`capabilities.json` の `emits[source]` で許可された event kind だけが受理されます。
policy の grantor に `*` は使えません。capability 値には `*` を使えますが、許可範囲が広がるため trusted source / extension だけに限定してください。
`source` と拡張 ID はローカルな論理名です。OS の sandbox ではありません。`spindle` は private state directory / Unix socket と policy を境界にする、同一ユーザー内の trusted local automation bus です。
peer UID 検査はまだ実装していません。socket と state directory の権限を private に保ち、同一ユーザー内で信頼できる client / extension だけを接続してください。

## manifest と登録 surface

`stdio-jsonl` manifest は、拡張 ID、version、entrypoint、runtime に加えて、static install する場合は event/action/capability surface も持ちます。
surface が空の manifest は dynamic registration が必要なので、`install --trust-runtime` または `extension register --trust-runtime` を使います。
static registration は install / register 時に entrypoint を実行しません。ただし stdio-jsonl extension の action を invoke する場合、entrypoint は実行されます。
entrypoint change detection が必要な場合は `--trust-runtime` で登録してください。

```json
{
  "id": "sketchybar",
  "version": "0.1.0",
  "entrypoint": "../../target/release/spindle-sketchybar",
  "runtime": "stdio-jsonl"
}
```

実際の event/action/capability は、manifest に静的に書くこともできますが、公式拡張では SDK を使って拡張コードから登録します。
`emits` は拡張が外部入力や IPC から観測して発火できる event kind、`produces` は拡張 action の `ActionOutput` が返せる event kind です。
どちらも event surface の所有権として扱われるため、同じ event kind を別拡張が `emits` / `produces` のどちらかで重複登録することはできません。

```rust
ExtensionRegistration::new()
    .emit("sketchybar.workspace.clicked")
    .produce("sketchybar.message.sent")
    .capability("sketchybar.ui.write")
    .action(
        "sketchybar.message.send",
        RegistrationAction::new().capability("sketchybar.ui.write"),
    )
```

ルートは event から action への小さな接続です。capability を grant する route は `source` 必須です。dispatch 時は、イベント payload と route の static `args` が object として merge されます。同じ key がある場合は route の `args` が優先されます。

```json
{
  "event": "sketchybar.workspace.clicked",
  "source": "sketchybar",
  "action": "aerospace.workspace.focus",
  "capabilities": ["aerospace.window.control"],
  "args": {}
}
```

## 非同期 continuation

route/direct invocation で拡張 action を呼ぶとき、daemon は `ActionContext` に短命の `ContinuationContext` を渡します。拡張は action response をすぐ返したあとでも、この handle を使って daemon socket に `continuation-invoke` / `continuation-emit` を送れます。

continuation は original invocation の capability grant に限定され、core が handle identity・origin extension・expiry・required capability を検証します。無効・期限切れ・capability 不足の continuation work は fail closed します。continuation-backed invoke は `action.requested` event に continuation provenance を記録します。

## stdio JSONL 拡張ホスト

`spindle-extension-sdk` は、カーネルと拡張ホストの間で使う型付き contract を提供します。

拡張ホストは stdin/stdout で次の request を受けます。

- `register` — `ExtensionRegistration` を返す
- `invoke` — `ActionInvocation` を受けて `ActionOutput` を返す
- `shutdown` — ホストを終了する

アクションは `ActionOutput` でイベントを返せます。返せる event kind は registration / manifest の `produces` に宣言します。
返されたイベントの `source` は拡張が指定するのではなく、カーネルが invoking extension id で付与します。返されたイベントはイベントログに保存され、さらに一致する route が dispatch されます。
同じ stdio host session に対する invocation は直列化されます。異なる extension の host session は別々に実行されるため、遅い extension が他の extension の invocation を塞がない設計です。

`ActionContext::extension()` には daemon が見ている installed surface が入ります。
workflow 拡張はこれを使って、必要な provider event や action があるかを実行時に確認できます。
`ActionContext::continuation()` には、deferred work を行うための短命 continuation handle が入ります。

## 公式拡張

### `aerospace`

AeroSpace の Unix socket IPC を扱います。

主な surface:

- emits: `aerospace.workspace.changed`, `aerospace.focus.changed`, `aerospace.monitor.changed`,
  `aerospace.mode.changed`, `aerospace.layout.changed`
- produces: `aerospace.workspace.snapshot`, `aerospace.mode.snapshot`, `aerospace.layout.snapshot`
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
AeroSpace snapshot を SketchyBar message request に変換し、`workspace-indicator.sketchybar.message.requested` を action output として produce します。

既定の workspace list は `1,2,3,4,5,6,7,8,9,10` です。

## 状態ファイル

state directory には主に次のファイルが置かれます。

- `events.jsonl` — append-only event log
- `extensions.json` — install/register 済み拡張
- `capabilities.json` — emit / direct / route grant policy
- `spindle.sock` — daemon の Unix socket

既定の state directory は `$SPINDLE_STATE_DIR` があればその値、なければ `$HOME/.local/state/spindle` です。

## ライセンス

MIT
