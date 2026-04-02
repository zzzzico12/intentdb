# IntentDB

スキーマ不要・自然言語で入れて自然言語で検索できるストレージエンジン。

SQLも型定義も不要。テキストを入れれば、意味で検索できる。

```
idb put "田中さんは2024年3月に商品Aを購入した"
idb search "最近問題があった顧客"
```

## コンセプト

従来のDBはスキーマ設計が前提。IntentDBは「何を保存したいか」だけ書けばいい。  
テキストはOpenAIのembeddingベクトルに変換され、独自バイナリ形式（`.idb`）で保存される。  
検索はコサイン類似度による意味検索。キーワードが一致しなくても文脈で引っかかる。

## インストール

```bash
git clone https://github.com/yourname/intentdb
cd intentdb
cargo build --release
# PATHに追加するか、直接 ./target/release/idb を使う
```

**必要なもの:**
- Rust 1.75+
- OpenAI APIキー（[platform.openai.com](https://platform.openai.com) で取得）

## 使い方

```bash
export OPENAI_API_KEY=sk-...

# データを追加
idb put "田中さんは2024年3月に商品Aを購入した"
idb put "佐藤さんから返品のクレームが来た"
idb put "来週の月曜にミーティングを設定した"

# 自然言語で検索（上位5件）
idb search "最近問題があった顧客"
idb search "スケジュール関連の記録"

# 検索件数を指定
idb search "購入履歴" --top 10

# 全件表示
idb list

# 削除（IDの先頭8文字で指定）
idb delete a1b2c3d4

# DBファイルを指定（デフォルト: data.idb）
idb --file mydata.idb put "テキスト"
```

## ファイルフォーマット（.idb）

独自バイナリ形式。SQLiteやJSONに依存しない。

```
[MAGIC: 4B "IDB1"]
[レコード数: u32 LE]
  [idの長さ: u16][id bytes (UTF-8)]
  [textの長さ: u32][text bytes (UTF-8)]
  [vector次元数: u32][f32 x N (LE)]
  [timestamp: u64 (unix seconds LE)]
  ...
```

## ロードマップ

- [x] `put` / `search` / `list` / `delete`
- [ ] HNSWインデックス（大規模データでの高速検索）
- [ ] メタデータ・タグによるフィルタリング
- [ ] AIによるintent自動分類
- [ ] Pythonバインディング（PyO3）
- [ ] HTTP API

## ライセンス

MIT
