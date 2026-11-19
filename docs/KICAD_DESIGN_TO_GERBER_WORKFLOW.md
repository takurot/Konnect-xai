# Konnectで回路図からガーバーまで作る総合ワークフロー

- **設計確認日** 2026-11-19
- **確認した実装** `556b48bb339d5abd3a17c3040062723ce9a015a0`
- **実機評価** PCB同期の適用で停止。配置以降は別基板で出力まで確認（[実機結果](#実機結果)）。

空のプロジェクトから単一シートの回路図を作り、同じ回路図からPCBの部品とネットを取り込み、配置・配線・ERC/DRC・ガーバー／ドリル出力までをMCPクライアントで順に実行する想定。KiCadの起動、PCB Editorでの対象基板のオープン、IPCの初期設定は操作者が担当する。初回評価の最後にKiCad Gerber Viewerで出力を確認する。

[KiCad実機E2E評価手順書](KICAD_E2E_RUNBOOK.md) の C01 は空基板からのガーバー出力とERC/DRC応答を、L08は回路図からPCBへの同期を検証する。本シナリオでは一つのプロジェクトで各工程をつなぎ、正しいネットが実際に銅箔でつながった基板から出力したことまで確認する。

## 実機回帰テスト

[配線済み基板の回帰テスト](../crates/konnect/tests/routed_kicad/run.py) は、同じ回路から正常な `routed` と `LOAD_B` を未配線にする `unrouted` を作る。Python 3.11以降、標準ライブラリ付きKiCad 10、対象コミットからビルドしたKonnectを使う。GUI操作が必要なため通常のCargoテストからは起動しない。

最初にスタンドアロンPCB Editorを閉じ、リポジトリのルートで実行する。以下はmacOSの例。評価環境に合わせてKiCad CLIのパスを変える。

```sh
scratch="$(mktemp -d /Volumes/Storage/private/konnect-routed.XXXXXX)"
export TMPDIR="$scratch/tmp"
export CARGO_TARGET_DIR="$scratch/target"
mkdir "$TMPDIR"
sha="$(git rev-parse HEAD)"
KONNECT_BUILD_COMMIT="$sha" cargo build --locked -p konnect
runner=crates/konnect/tests/routed_kicad/run.py
work="$scratch/evidence"
python3 -B "$runner" prepare --work-dir "$work" \
  --konnect "$CARGO_TARGET_DIR/debug/konnect" \
  --kicad-cli /Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli \
  --expect-sha "$sha"
```

`prepare` は出力先を新規作成し、両回路のERCとKiCad XMLネットリストのピン所属を確認する。準備後は各PCBを別のPCB Editorセッションで一枚ずつ開く。Schematic Editorは閉じたままとし、テスト時は手動編集・手動保存を行わない。

1. IPCを有効にしたPCB Editorで `$work/routed/load-test.kicad_pcb` を開く。[実機E2E手順のライブIPC設定](KICAD_E2E_RUNBOOK.md#b-guiとライブipcの評価) に沿って、そのPCB Editorのソケットを `KICAD_API_SOCKET` に指定する。
2. 下の `routed` コマンドを実行し、結果を記録する。実行中は同じPCB Editorを開いたままにする。
3. 最初のPCB Editorを閉じ、`$work/unrouted/load-test.kicad_pcb` を新しく開く。そのPCB Editorのソケットに `KICAD_API_SOCKET` を設定し直して `unrouted` コマンドを実行する。

```sh
python3 -B "$runner" run --work-dir "$work" --case routed --ipc "$KICAD_API_SOCKET"
# routedの結果が記録されたらPCB Editorを閉じ、別セッションでunroutedを開く。
python3 -B "$runner" run --work-dir "$work" --case unrouted --ipc "$KICAD_API_SOCKET"
```

書き込み前に読み取り専用の要求で対象PCBが一枚だけ開いていることを確認する。起動直後の応答は既定25秒まで待機し、別の基板が見えた場合は失敗する。空PCBを最初に保存してKiCadの既定設定を書き出し、その後に同期・配置・配線を行う。最終保存の後、DRCと出力の前後でPCB・回路図・プロジェクトのハッシュを確認する。

両ケースの `result.json` が `pass` で合格。`routed` のDRCは全カテゴリが0件となり、`routed/gerbers/` に出力される。`unrouted` ではDRCの `unconnected_items` ルールが対象パッドに1件発生し、出力の呼び出しも `unrouted/gerbers/` の作成も行われない。ファイル検証はこの回路でKiCadが出力するGerberとExcellonの形式を対象にする。

`environment.json`、`prepare/`、`run-routed/`、`run-unrouted/` に、対象SHA、KiCadバージョン、MCP要求と応答、入力ハッシュ、出力検証を保存する。失敗時もログを残し、同一ケースへの上書き実行を拒否する。再実行は新規の作業領域で `prepare` から行う。Gerber Viewerの目視確認は、日時・使用バージョン・対象ファイルとともに `run-routed/viewer.md` などへ別途記録する。

GUIを使わない受け入れ判定のテストは次で実行する。

```sh
python3 -B -m unittest discover -s crates/konnect/tests/routed_kicad
```

## 回路と基板

2ピンヘッダーの両端に10 kΩの抵抗を接続する受動回路。電源シンボル、未接続ピン、階層シート、ビア、ゾーンを使わず、ネット同期と配線を2本の接続で確認する。標準ライブラリの候補は次のとおり。

| 参照 | シンボル | フットプリント |
|---|---|---|
| J1 | `Connector_Generic:Conn_01x02` | `Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical` |
| R1 | `Device:R`（値 `10k`） | `Resistor_THT:R_Axial_DIN0207_L6.3mm_D2.5mm_P7.62mm_Horizontal` |

| 論理ネット名 | 接続するピン／パッド |
|---|---|
| `LOAD_A` | J1.1、R1.1 |
| `LOAD_B` | J1.2、R1.2 |

基板は20 × 20 mm、2層、表面配置、`F.Cu` のみで配線する。暫定線幅は0.30 mmとし、実行時にプロジェクトのルールとの整合を確認する。

## 実行前の準備

- KiCad 10のPCB Editor、同じインストールの `kicad-cli`、標準シンボル／フットプリント、Konnectのstdio MCP接続を用意する。CLIの探索方法とPCB EditorのIPC設定は[実機E2E手順](KICAD_E2E_RUNBOOK.md#前提と証跡の準備)を参照する。`get_installation_info` でMCPサーバーからCLIが見えていることを確認し、KiCadのパッチバージョン、KonnectのSHA、ライブラリの出典を記録する。CLIを検出できない場合は [Konnectの起動設定](KICAD_INTEGRATION.md) の `kicad_cli` に絶対パスを指定する。
- `/Volumes/Storage/private` 配下に毎回新規の実行ディレクトリを作る。設計ファイル、報告、製造出力の置き場を分け、MCPには絶対パスを渡す。評価用のPCBだけを開き、実行中の手動編集・並列更新を止める。Schematic Editorは回路図の編集工程中は閉じておく。
- 必要に応じて `load_toolset` で `project`、`sch_components`、`sch_wiring`、`sch_export`、`pcb_board`、`pcb_components`、`pcb_routing`、`placement`、`verification`、`pcb_export` をロードする。起動したMCPの `tools/list` で引数を確認する。[ツール一覧](../tool-directory.md)

## 実行順と合格条件

| 工程 | 実行 | 次に進む条件と証跡 |
|---|---|---|
| 1. 新規作成 | `create_project` に実行ディレクトリ内の新規 `load-test/` と `name: "load-test"` を渡す。 | 同名の `.kicad_pro`、`.kicad_sch`、`.kicad_pcb` がそろう。入力には以後この3ファイルを使う。 |
| 2. 回路図 | `add_schematic_component` で2部品を配置し、フットプリントを明示する。左に縦向きのR1、右にピンが左を向くJ1を置く。 下記の回路図結線案に従う。 | `list_schematic_components` で2部品の参照とフットプリントを確認し、`export_schematic_svg` で読みやすさと意図しない交差がないことを確認する。 |
| 3. ERC・ネットリスト | `run_erc` と `generate_netlist` を同じ保存済み回路図に対して実行し、それぞれ `output` に保存先を指定する。ERCの `severity` は `warning` とする。 | ERCはエラー・警告0件。KiCadが生成したネットリストのノード集合が上の表に一致し、追加・欠落・短絡がない。ローカルネット名にはシートの接頭辞が付く場合があるため、名前とピン所属をセットで記録する。 |
| 4. PCBへの同期 | 操作者がプロジェクトをPCB Editorで開く。 `open_project` に対象PCBの絶対パスを渡し、`requested_open: true` と `requested_board` の一致を確認。 `update_pcb_from_schematic` をdry-runし、同じ対象へ `dry_run: false` と `expected_plan_revision` を指定して適用する。 | 初回 `ready` で `coverage.footprints_added.planned: 2`（J1・R1）、`unassigned_footprints` は空、`coverage.conflicts.planned` と `coverage.skipped_by_flag.planned` は0件。適用が `applied` で `coverage.footprints_added.applied: 2`、再dry-runが `noop`。 `get_component_pads`（`source: "ipc"`）と `get_nets_list` で両ネットに正しいパッドだけが付いていることを確認する。 |
| 5. 外形・配置 | `add_board_outline` で `(106,102)`〜`(126,122)` mmの角丸なし外形を一度追加する。 `set_component_placements` で下記の配置を適用し、`get_component_pads`、`get_layer_list`（`board_source: "live"`）、`score_placement` でliveの状態を読む。 | 指定の2銅層と、配置評価の `source: "ipc"`、`outline_shape: "rectangular"`、空の `hard_failures` を確認する。実パッド位置と配線計画を保存する。 |
| 6. 配線 | 実パッド位置からネットごとの折れ線を決め、`route_trace` を線分ごとに直列に呼ぶ。 `net_name` はliveで読んだ完全名を使用する。 | `query_traces` をネットと `F.Cu` で絞り、始終点・線幅・層・線分IDを計画と照合する。各パッド中心に到達し、他ネットとの交差・余分な線分がない。 |
| 7. 保存・DRC | 変更を止め、回路図・プロジェクトのハッシュを控える。 `run_drc` に `sync_live_board: true`、`refill_zones: false`、報告用の `output` を指定する。 | `live_board_synced: true`、エラー・警告・設計ルール違反・`unconnected_items`・`schematic_parity` がすべて0件。必須カテゴリの欠落や `null` があれば停止する。DRC前後で回路図・プロジェクトのハッシュが一致することを確認し、DRCを通過した保存済みPCBのハッシュを記録する。[保存とDRCの契約](DRC_SYNCHRONIZATION.md) |
| 8. 製造出力 | DRC後に設計ファイルを変更せず、`export_gerber` を新しい出力ディレクトリに対して実行する。 | 入力ハッシュがDRC時と出力前後で一致。下記の出力検査がすべて通る。 |

### 回路図の結線案

R1を `(101.6,101.6)`、J1を `(127,101.6)` mmに置く。 `get_schematic_pin_locations` のピン先端の座標と向きを読み、ピン番号で接続相手を選ぶ。R1.1からは上、R1.2からは下へ1グリッド（1.27 mm）延ばし、折り曲げ用のX座標 `114.3` mmまで水平に進む。そこから各J1ピンのY座標まで垂直に進み、J1の先端へ水平につなぐ。

実座標で部品や別ネットに触れないことを確認してから、水平・垂直線分を `batch_add_wire` の `wires` にまとめて渡す。各ネットの折れ点に `add_schematic_net_label` で同名のローカルラベルを一つずつ置く。使用ライブラリでピン方向や位置が想定と異なる場合は実座標を使って経路を組み直す。

### 配置・配線の初期案

座標はPCB上のmm。R1のパッド1を `(112,108)`、J1のパッド1を `(114.54,116)` に置き、両フットプリントのパッド2がパッド1からX正方向に並ぶ向きにする。 `set_component_placements` にはフットプリント原点と絶対回転を渡すため、配置後のパッド座標で向きと原点の前提を確認する。

| ネット | パッド位置が想定どおりの場合の折れ線 |
|---|---|
| `LOAD_A` | R1.1 `(112,108)` → `(112,110)` → `(114.54,112.54)` → J1.1 `(114.54,116)` |
| `LOAD_B` | R1.2 `(119.62,108)` → `(119.62,110)` → `(117.08,112.54)` → J1.2 `(117.08,116)` |

基準形状と異なる場合は配置または折れ線を見直し、読み直したパッド中心に合わせる。ネット名・端点・幅を確定してから書き込む。 `route_trace` は指定線分を作成するため、クリアランスや導通の最終判定はDRCに委ねる。固定形状の評価では外部ルーターを導入せず、APIで制御できる6線分から始める。

### ガーバーとドリルの確認

`export_gerber` の `layers` は `["F.Cu","B.Cu","F.Mask","B.Mask","F.SilkS","B.SilkS","Edge.Cuts"]`、`drill_file` は `true`。戻り値の `files` と出力実体を照合し、要求の7層がそろい、各ファイルが非空であること、ExcellonのPTHドリルが存在することを機械検査する。KiCadは `.gtl` など層別の拡張子も使うため、各Gerberの `TF.FileFunction` と `.gbrjob` の層一覧を照合する。裏面シルクに描画要素がなくても、対応する有効なGerberファイルがあればよい。

初回はKiCad Gerber Viewerで銅箔・マスク・外形・ドリルを重ね、以下を記録する。

- `F.Cu` に独立した2経路があり、`B.Cu` にはスルーホールのパッドが見える。裏面トラックは0本。
- 20 × 20 mmの閉じた外形の中に、両面のパッド／マスクと位置が重なる4つのPTH穴がある。余分な穴がない。
- 出力一式、層対応表、確認画像、DRCと出力元のハッシュを、ERC／ネットリスト／配置・配線の取得結果と同じ実行記録に残す。

## 中断・再開と自動化の境界

- MCP応答は `isError` に加え、ツール固有のステータスと実データを評価する。IPC対象不一致、同期競合、ライブラリ不足、ERC/DRC違反、出力不足では次工程を止める。違反を直したら、影響を受ける最初の工程から検証を再実行する。
- タイムアウト、部分成功、`mutation_outcome_uncertain` では即時再送しない。直前の要求と応答、liveと保存済みの状態を分けて保全し、同期計画や線分一覧と照合する。計画の再取得、残りの操作だけの適用、または新規プロジェクトから再試行のいずれかを状態確認後に選ぶ。出力失敗時の再試行は新規出力ディレクトリを使う。
- 初回は上記の固定配置・配線で、MCPオーケストレーターから全工程を一度実行し、成功後に別の新規ディレクトリで再現性を確認する。負の対照として別の評価用コピーで片方のネットを未配線にし、DRCで検出して出力工程を起動しないことも確認する。
- 合格条件は回路図から出力までの一連の実行記録とGerber Viewerでの照合がそろうこと。操作者が行う起動・オープンと初回の目視確認も所要時間として記録する。完全無人運転を評価する場合はPCB Editorの起動と対象接続の管理、およびGerber／ドリルを読み取る自動検査が追加で必要になる。

## 実機結果

2026-11-19、macOS arm64、KiCad 10.0.8。同一SHAからビルドしたKonnectにstdio MCPで接続し、試験用のKiCad設定にだけIPCを有効化した。PCB EditorのPIDが保持するソケットと `open_project.requested_open` で対象基板を確認した。

| ケース | 結果 | 確認した内容 |
|---|---|---|
| 通し試験と新規プロジェクトでの再試行 | **PCB同期で停止（2回とも）** | 回路図作成、ERCエラー・警告0件、2ネットと全ピンの所属一致。同期のdry-runは `ready` でJ1・R1の追加を計画した。適用は `ipc_outcome_unknown`、`cannot verify native assembly attributes for missing footprint J1`、`retry_safe: false`。失敗後のlive基板でフットプリントとトラックは0件だが、元の保存済み空基板にはない2ネットが残った。同じ基板への再適用は行わず、通し試験では出力工程を起動しなかった。 |
| 後続工程の切り分け | **配置・配線・DRC・製造出力を確認** | 別の評価用基板にKiCad同梱Python APIで、同じ実ネットリストからフットプリント・回路図へのリンク・パッドネット・Descriptionを取り込んだ。その基板を対象にKonnectが外形を作成し、2部品を配置し、表面に6線分を追加。live保存後のDRCはエラー・警告・設計ルール違反・未配線・回路図差分がすべて0件。入力ハッシュが一致した状態でガーバー7層とExcellonを出力。ガーバー／ドリルの実ファイルで外形20 × 20 mm、表面2経路、裏面トラック0本、両面のパッド／マスクに一致するPTH穴4個を確認した。新規の評価用基板でも後続工程を再現できた。 |
| 未配線の対照 | **出力ゲートで停止** | 別基板で `LOAD_B` の配線だけを省略。DRCで未配線1件、回路図差分0件。出力ディレクトリは作成されなかった。 |

同期の失敗は、部品追加コマンド後、KiCadのIPCコミット内で行う組立属性の読み戻しで発生した。読み戻しにJ1が現れない原因は未確定。製造出力の幾何はファイルから照合した。Gerber Viewerでの目視確認は、画面を取得できない環境だったため未実施。
