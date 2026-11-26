# FreeBSD NAS ローカルスキャン手順（JSONL マニフェスト連携）

大量の写真が保存された FreeBSD NAS 上で `wingpv` をローカル実行し、SMB 越しに全量ハッシュ計算することなく Windows 側のカタログ DB へ取り込む手順です。

## 1. 事前準備（FreeBSD ホスト側）

Rust / Git / ExifTool（旧型 CR2・CRW・動画用）/ tmux を導入します。

```sh
sudo pkg install rust git p5-Image-ExifTool tmux
```

リポジトリを取得してリリースビルドします。

```sh
git clone https://github.com/yshdsnd/wing-photovault.git
cd wing-photovault
cargo build --release --bin wingpv
```

## 2. NAS 用設定ファイル（例: `nas.toml`）の作成

`root` には NAS ホスト上のローカルパス（例: `/tank/photos`）を指定します。  
`id`（例: `nas_backup_eagle`）は Windows 側の `wing-photovault.toml` と揃えておくと取り込み時に自動対応します。

```sh
cat << 'EOF' > nas.toml
[host]
id = "nas-freebsd"
name = "FreeBSD NAS"

[general]
db_path = "./nas-scan.db"

[performance]
hdd_io_workers = 1
db_batch_size = 500

[exif]
backend = "auto"
persistent_exiftool = true

[[storages]]
id = "nas_backup_eagle"
root = "/path/to/photos"
media = "hdd"
tier = "backup"
access_mode = "read_only"
backup_mode = "external"
failure_domain = "nas-eagle"
EOF
```

## 3. 動作確認（少量テスト）

まず 100 ファイルだけ試して、パス・EXIF・インターバル動作を確認します。  
（`--max-files` 指定時は未走査ファイルを `missing` 扱いにしません）

```sh
./target/release/wingpv --config ./nas.toml init
./target/release/wingpv --config ./nas.toml scan --max-files 100 --cooldown-every 50 --cooldown-secs 5
./target/release/wingpv --config ./nas.toml stats
```

## 4. 長時間スキャンの実行（インターバル付き・SSH 切断耐性）

HDD の連続負荷・発熱を抑えるため `--cooldown-every` と `--cooldown-secs` を指定し、`tmux` 内で実行します。  
休止前に必ず SQLite へバッチコミットされるため、途中で中断しても次回 `scan` で未処理分から自動再開されます。

```sh
tmux new -s wingpv
./target/release/wingpv --config ./nas.toml scan --cooldown-every 500 --cooldown-secs 30 2>&1 | tee nas-scan.log
```

- デタッチ（バックグラウンド継続して SSH ログアウト可）: `Ctrl+b` のあと `d`
- 再接続して進捗確認: `tmux attach -t wingpv`
- 別シェルから現在の登録状況を確認:
  ```sh
  ./target/release/wingpv --config ./nas.toml status
  ./target/release/wingpv --config ./nas.toml stats
  ```

## 5. マニフェスト書き出し（FreeBSD NAS 側）

スキャン完了後（または途中段階でも可）、DB から軽量な JSONL マニフェストを書き出します。

```sh
./target/release/wingpv --config ./nas.toml export-manifest \
  --storage nas_backup_eagle \
  --output nas-manifest.jsonl
```

## 6. Windows PC 側への取り込み

`nas-manifest.jsonl` を Windows PC へコピー（または共有経由で参照）し、`import-manifest` を実行します。

```powershell
# Windows 側の wing-photovault.toml に同じ storage id (例: nas_backup_eagle) が定義されている状態で実行
.\target\release\wingpv.exe import-manifest .\nas-manifest.jsonl

# バックアップ済みコンテンツ数（Backed-up Contents）を確認
.\target\release\wingpv.exe stats
```

※ マニフェスト内の `storage_id` を別名に割り当てたい場合は `--storage <Windows側のstorage_id>` を付けて実行します。
