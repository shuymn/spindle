# spindle

[English](README.md)

`spindle` は、macOS のローカル自動化をイベント・アクション・拡張でつなぐ小さなハーネスです。

コアは意図的に小さく保っています。イベントを保存し、拡張の登録情報を検証し、イベントからアクションへのルートを dispatch します。アプリ連携、外部ツール連携、ワークフローのロジック、エージェントのフックは、カーネル外の拡張として実装します。

```text
イベントを受け取る
  -> append-only JSONL ログに保存する
  -> インストール済みルートを探す
  -> 拡張アクションを呼ぶ
  -> アクションが出したイベントをさらに dispatch する
```

## 状態

実験中です。このリポジトリには daemon カーネルと拡張 SDK が含まれています。

## リポジトリ構成

```text
spindle/
  crates/spindle/                    daemon カーネル、CLI、socket server
  crates/spindle-extension-sdk/      stdio JSONL 拡張ホスト用の型付き SDK
  crates/spindle-extension-example/  最小 stdio JSONL 拡張のサンプル
  docs/                              コンセプト、使い方、開発メモ
```

## Quick start

Rust は `rust-toolchain.toml` で固定されています。

```bash
cargo build --workspace --locked
cargo test --workspace --all-targets --all-features --locked
```

Task が使える場合はこちらも利用できます。

```bash
task build
task test
task check
```

実験中は明示的な state directory を使うと安全です。

```bash
export SPINDLE_STATE_DIR=/tmp/spindle
mkdir -p "$SPINDLE_STATE_DIR"
chmod 700 "$SPINDLE_STATE_DIR"

cargo run -p spindle -- emit \
  --type agent.status.changed \
  --source pi \
  --data '{"state":"working","message":"cargo test"}'

cargo run -p spindle -- query events --type agent.status.changed
```

## セキュリティモデル

拡張パッケージをインストールすることは、その実行コードを自分のユーザー権限で動かすと信頼することです。ユーザーの spindle socket に到達できるローカルクライアントは event を emit できます。event の `source` は routing label であり、認証主体ではありません。manifest route の `source` は、インストール済みでその event を所有する extension と照合されます。capability が必要な action 実行は、別の policy file ではなく、インストール済み route 宣言と continuation grant で認可します。

## ドキュメント

- [Concepts](docs/concepts.md) — カーネルの責務、拡張境界、コアに含めないもの
- [Usage](docs/usage.md) — setup、CLI 例、trusted local automation model、状態ファイル
- [Extensions](docs/extensions.md) — manifest、登録 surface、route、continuation、stdio JSONL host
- [Extension SDK README](crates/spindle-extension-sdk/README.md) — SDK package notes

Agent 向け開発メモ:

- [Coding guidelines](docs/coding.md)
- [Testing guidelines](docs/testing.md)
- [Tooling guidelines](docs/tooling.md)
- [Review guidelines](docs/review.md)

## ライセンス

MIT
