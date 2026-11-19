# KiCad実機E2E評価手順書

## 目的と今回の結果

KonnectのMCP呼び出しから、KiCadが読める保存ファイル・CLI出力・開いている基板への変更までを確認する。対象ツールの全一覧は [tool-directory.md](../tool-directory.md)、実装上の境界は [KICAD_INTEGRATION.md](KICAD_INTEGRATION.md) を参照する。この評価表は主要経路とv0.15.0の受け入れ項目を対象とし、全ツールの網羅を意味しない。

**2026-11-17時点、このMacにはKiCadが未導入のため、ローカル実機評価は全項目「未実施」。** 今回は手順と既存テストの対応をソースで確認した。後述するWindows CIの過去実績はログを再確認した参考情報であり、このMacや本ドキュメントのPRで実行した結果ではない。

| 評価経路 | 実際に確認できること | 確認できないこと |
|---|---|---|
| CLI E2E | Konnect実行バイナリのstdio MCP、保存データ、実際の`kicad-cli`による解釈・出力 | PCB Editorのライブ操作・Undo・画面表示 |
| GUI + live IPC | 開いた対象基板、実ライブラリ配置、変更後の再取得、保存・再オープン | 実行していない他OS・別KiCadバージョン |
| mock IPC / fixture | 要求内容・異常応答・拒否・復旧条件の再現 | 実KiCadによる受理・GUIの動作 |

## 前提と証跡の準備

1. KiCad 10、標準シンボル／フットプリントライブラリ、デモ回路図、Rustと`protoc`を準備する。ビルド要件は [DEV.md](../DEV.md)。既存CIはKiCad **10.0.5**を指定するが、native flip／3Dモデルのliveテストは**10.0.6以降**が必要。使用バージョンを記録し、必要機能がない環境は「環境待ち」とする。
2. fixtureは必ず作業領域へコピーする。追跡中のfixtureや本番設計を直接GUIで開かない。KiCadは保存時にファイルを更新し、関連プロジェクト・ロック・履歴も作成する。
3. 各GUIケースは新しいコピーから開始する。同じ基板へのテストは直列実行し、期待する基板とソケットをケースごとに確認する。GUIを閉じてからコピーを破棄する。

以下はmacOSで導入後に使うbash例。リポジトリのルートから実行する。パスはインストール先に合わせて変更する。**KiCad未導入の現状では実行を進めない。**

```bash
set -euo pipefail
export E2E_REPO="$(git rev-parse --show-toplevel)"
export E2E_ROOT="$(mktemp -d /Volumes/Storage/private/konnect-kicad-e2e.XXXXXX)"
mkdir -p "$E2E_ROOT/logs" "$E2E_ROOT/tmp" "$E2E_ROOT/artifacts"
export TMPDIR="$E2E_ROOT/tmp"
export CARGO_TARGET_DIR="$E2E_ROOT/cargo-target"
export KICAD_CLI="/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
export KICAD_DEMOS="/Applications/KiCad/KiCad.app/Contents/SharedSupport/demos"
test -x "$KICAD_CLI"
export PATH="$(dirname "$KICAD_CLI"):$PATH"
command -v kicad-cli
kicad-cli --version
command -v protoc
python3 - <<'PY'
import os
from pathlib import Path
root = Path(os.environ['KICAD_DEMOS'])
assert root.is_dir() and next(root.rglob('*.kicad_sch'), None), root
PY
{
  date -u
  git rev-parse HEAD
  git status --short
  uname -a
  rustc --version
  cargo --version
  protoc --version
  "$KICAD_CLI" --version
} > "$E2E_ROOT/environment.txt"
export KONNECT_E2E_KEEP_DIR="$E2E_ROOT/artifacts/design-loop"

# pipefailによりcargo失敗をtee成功で隠さない。
run_case() {
  local case_id="$1"
  shift
  "$@" 2>&1 | tee "$E2E_ROOT/logs/$case_id.log"
}
```

`KICAD_CLI`だけではvariantテストに届かないため、CLIのディレクトリを`PATH`にも追加する。デモ不在のconformanceテストには、早期returnでテスト自体が`ok`になる経路がある。C07はCLI未検出時に失敗する。**終了コード0や`0 ignored`だけで合格にしない。** 各ログの`SKIP`・`skipping`、対象テスト名、実処理件数も読む。

## A. KiCad CLIとMCPの自動評価

出典は [e2e_kicad.rs](../crates/konnect/tests/e2e_kicad.rs)。C01〜C05はKonnectの実行バイナリへstdio MCPで呼び出す。シェル関数は上記と同じセッションで使う。

| ID | 機能・テスト名 | やり方と合否基準 | このMac |
|---|---|---|---|
| C01 | 設計ループ：`full_design_loop_with_real_kicad` | project作成→R/C/レギュレータ配置→結線・VINラベル→ERC→netlist→design review→Gerber→DRC。KiCad netlistのVINにR1.1が所属し、部品数がreviewと一致、BOMのDescription/Datasheetが保持、Gerberが生成されること | 未実施 |
| C02 | native属性：`native_assembly_attributes_survive_real_kicad_bom_export` | ECC83 fixtureのU1全3ユニットをDNP化。`--exclude-dnp`のBOMからU1が消え、R1-R4は残る。MountingHoleの保存インスタンス／埋込ライブラリが`in_bom=no, on_board=yes`であること | 未実施 |
| C03 | PCB SVG：`selected_layer_pcb_svg_export_through_mcp` | 指定層のみのSVGと複数層SVGを出力。既知の描画座標が対象層にだけ存在し、SVGが成立すること | 未実施 |
| C04 | 保存互換性：`structural_scan_fixture_reloads_through_real_kicad` | 編集fixtureをKiCadでupgrade・netlist出力。成功し、R1/R3が残ること | 未実施 |
| C05 | 配線描画：`a_written_default_netclass_still_plots_wires` | Default netclassを更新してSVG化。配線のstrokeとjunction半径が維持され、追加クラスでDefaultが壊れないこと | 未実施 |
| C06 | デモ互換性：`conformance_test` | KiCad同梱デモを4テストでparse・構造抽出・編集往復・ラベル方向評価。デモが0件でなく、全件parseされ、SKIPがないこと | 未実施 |
| C07 | variant：`kicad_cli_still_honors_the_variant_clause` | Default・Lite・LiteのDNP除外を実CLIで出力。CSVの値とR1だけが除外されることを照合し、値・DNP句を個別に壊したコピーも検出する。DNP欄の表記はKiCadの言語設定に従う | 未実施 |
| C08 | 応答停止：`wedged_server_times_out_instead_of_hanging` | 応答しない**mock**サーバーに対して25秒以上60秒未満でエラーとなること。実KiCadのハング再現ではない | 未実施 |
| C09 | PCMパッケージ | CIのAssemble + schema-validate PCM packageを実施し、生成ZIPがvalidatorに受理されること。GUIでのインストール・起動は別途確認する | 未実施 |

```bash
# 各ケースが1 passedであることを個別に記録する。
run_case C01 cargo test -p konnect --locked --test e2e_kicad full_design_loop_with_real_kicad -- --ignored --exact --nocapture
run_case C02 cargo test -p konnect --locked --test e2e_kicad native_assembly_attributes_survive_real_kicad_bom_export -- --ignored --exact --nocapture
run_case C03 cargo test -p konnect --locked --test e2e_kicad selected_layer_pcb_svg_export_through_mcp -- --ignored --exact --nocapture
run_case C04 cargo test -p konnect --locked --test e2e_kicad structural_scan_fixture_reloads_through_real_kicad -- --ignored --exact --nocapture
run_case C05 cargo test -p konnect --locked --test e2e_kicad a_written_default_netclass_still_plots_wires -- --ignored --exact --nocapture
run_case C06 cargo test -p konnect-core --locked --test conformance_test -- --nocapture
run_case C07 cargo test -p konnect-sexp --locked --test variant_fixture_test kicad_cli_still_honors_the_variant_clause -- --ignored --exact --nocapture
run_case C08 cargo test -p konnect-ipc --locked --test mock_server_test wedged_server_times_out_instead_of_hanging -- --ignored --exact --nocapture
```

C01は不完全な小回路と空基板を使うため、ERC/DRCの構造化結果を得る試験であって、違反0件・配線済み基板・製造可能性の証明ではない。C02のMountingHole属性確認も、ライブ基板への同期完了を意味しない。C01のプロジェクトは`KONNECT_E2E_KEEP_DIR`に残るが、他ケースの一時生成物は自動削除される。詳細を残したい場合は次節の手動評価で明示した出力先へ保存する。

### Windows CIでの再実行とC09

[e2e-kicad.yml](../.github/workflows/e2e-kicad.yml) がC01〜C09を実行する。GitHub CLIを使える環境では以下を行う。新規runのIDを一覧から選び、**headShaが指定refのコミットと一致するか確認してから**ログを取得する。

```bash
gh workflow run e2e-kicad.yml --repo takurot/Konnect-xai --ref <評価ブランチまたはタグ>
gh run list --repo takurot/Konnect-xai --workflow e2e-kicad.yml --limit 5
gh run view <run-id> --repo takurot/Konnect-xai --json headSha,status,conclusion,jobs,url
gh run view <run-id> --repo takurot/Konnect-xai --log > "$E2E_ROOT/logs/hosted-e2e.log"
```

PRでは`run:e2e-kicad`ラベルを**付けるイベント**でも起動できる。既に付いているだけでは新しいpushの検証にならない。CIは失敗時のみ`dist/`と`e2e-artifacts/`をuploadするため、成功runでもログを保存する。C08が成功していてもGUIテストが実行されたことにはならない。

## B. GUIとライブIPCの評価

KiCadのPCB EditorでAPIを有効にし、**破棄可能なコピーだけを1枚**開く。一部の低レベルIPCテストはsole-board前提のため、別基板を使う追加確認とはセッションを分ける。APIが渡す`KICAD_API_SOCKET`と、必要な場合の`KICAD_API_TOKEN`を同じテストプロセスへ渡す。トークン値を結果表やログへ貼らない。詳細は [KiCad integration](KICAD_INTEGRATION.md#kicad-ipc) と [live_kicad_test.rs](../crates/konnect-ipc/tests/live_kicad_test.rs)。

macOSのソケットはフレームごとに異なり、managerのソケットがPCB Editorのものとは限らない。ファイル名の新しさだけで決定せず、L01のopen-document結果にコピーの絶対パスが含まれることを確認する。ソケットパスにはテストが受け取るNNGアドレス形式（例：`ipc:///実際のソケットパス`）を使う。

```bash
# L01〜L04の初期fixture。ケースごとに別ディレクトリへコピーして開き直す。
mkdir -p "$E2E_ROOT/L01"
cp "$E2E_REPO/crates/konnect-ipc/tests/fixtures/live_ipc.kicad_pcb" "$E2E_ROOT/L01/"
export KONNECT_LIVE_KICAD_BOARD="$E2E_ROOT/L01/live_ipc.kicad_pcb"
# 前ケースの任意overrideを残さない（L05は後でP3に設定）。
unset KONNECT_LIVE_KICAD_REFERENCE KONNECT_LIVE_KICAD_FOOTPRINT
unset KONNECT_LIVE_KICAD_PLACE_REFERENCE KONNECT_LIVE_KICAD_HOLE_REFERENCE
# このファイルをPCB Editorで開いてから、正しいソケットを設定する。
export KICAD_API_SOCKET="ipc:///PCB-Editorの実ソケットパス"

# 以下は関数定義。CASE_IDとTEST_NAMEは下表から選び、1件ずつ実行する。
run_mcp_live() {
  run_case "$1" cargo test -p konnect --locked --test live_kicad_tools "$2" -- --ignored --exact --nocapture --test-threads=1
}
run_ipc_live() {
  run_case "$1" cargo test -p konnect-ipc --locked --test live_kicad_test "$2" -- --ignored --exact --nocapture --test-threads=1
}
run_mcp_live L01 real_kicad_open_documents_resolve_to_comparable_paths
```

| ID | 実行方法（上の関数にIDとテスト名を渡す） | 準備・合否基準 | このMac |
|---|---|---|---|
| L01 | `run_mcp_live` / `real_kicad_open_documents_resolve_to_comparable_paths` | fixtureコピーを開く。実際のopen-documentが要求したboardへ解決されること | 未実施 |
| L02 | `run_mcp_live` / `board_extents_live_and_saved_agree_on_an_unedited_board` | 開いたコピーを保存後、未編集で実行。liveのbounds出典がIPC、savedの出典がファイル。列挙結果（空、拒否、復号不能）と開示が一致し、savedの範囲がliveに含まれる。測定対象外がなければ境界が一致 | 未実施 |
| L03 | `run_ipc_live` / `moving_and_rotating_footprint_preserves_child_geometry` | 新しいlive_ipcコピー。既定参照MH1を移動・回転し、子要素の幾何と保存後の内容が保持されること | 未実施 |
| L04 | `run_ipc_live` / `adding_a_via_actually_creates_it_on_the_board`、別コピーで`adding_a_zone_creates_it_on_the_live_board` | GND/SIG1と外形を持つlive_ipcコピー。実基板のvia／zoneを再取得し、追加内容が確認されること | 未実施 |
| L05 | `run_ipc_live` / `native_flip_round_trip_preserves_models_and_unrelated_footprint_content` | KiCad 10.0.6以降。F.Cu上にP3を用意し、3Dモデルに非ゼロのY offsetまたはX/Y rotationを設定。`KONNECT_LIVE_KICAD_REFERENCE=P3`を設定。F→B→Fでモデル・無関係な内容が戻ること。前提の詳細はテスト本体参照 | 未実施 |
| L06 | `run_mcp_live` / `place_component_loads_real_library_geometry` | 新しいコピーと標準Resistor_SMDライブラリ。R900配置後に実pad形状・位置・保存結果が一致すること | 未実施 |
| L07 | `run_mcp_live` / `add_mounting_hole_round_trips_through_live_kicad` | 新しいコピーと標準MountingHoleライブラリ。H900の追加を実IPCと保存データで確認すること | 未実施 |
| L08 | `run_mcp_live` / `schematic_sync_apply_then_dry_run_is_noop` | 下記同期用project。dry-runの`ready`→その`plan_revision`を指定したapplyの`applied`→再dry-runの`noop`が成立すること | 未実施 |
| L09 | `run_mcp_live` / `footprint_library_update_apply_then_dry_run_is_noop` | 新しい空基板と同名の`.kicad_pro`を保存。テストがproject内に専用ライブラリを作成・登録・更新する。変更反映と再計画noopを確認 | 未実施 |
| L10 | 下記Specctra Undoコマンド | 専用fixtureとFreerouting。1回のGUI Undoでインポート前のIPC snapshotへ戻ること | 未実施 |

L08はKiCadで新しい評価用projectを作り、回路図へR1を1個配置して実在するフットプリント（例：`Resistor_SMD:R_0402_1005Metric`）を割り当て、保存してSchematic Editorを閉じる。PCBは空のまま同名project内に保存し、PCB Editorで開いておく。`KONNECT_LIVE_KICAD_BOARD`にそのPCB、`KONNECT_LIVE_KICAD_SCHEMATIC`に閉じたルート回路図の絶対パスを設定して実行する。既に同期済みの基板では初回`ready`を確認できないため、新しいコピーを使う。

L10は [既存のUndo手順](TESTING_AND_RELEASE.md#evidence-focused-regression-tests) と同じ評価。`crates/konnect-core/tests/fixtures/specctra_two_resistors_locked.kicad_pcb`を別ディレクトリへコピーして開き、次を設定する。

```bash
export KONNECT_LIVE_SPECCTRA_BOARD="/コピー先/specctra_two_resistors_locked.kicad_pcb"
export KICAD_CLI_PATH="$KICAD_CLI"
export FREEROUTING_JAR="/インストール先/freerouting-2.3.0.jar"
run_case L10 cargo test -p konnect-core --locked one_undo_restores_the_exact_pre_import_board_snapshot -- --ignored --nocapture --test-threads=1
```

`LIVE_UNDO_READY`が出た時点でPCB EditorのUndoを1回だけ実行する（macOSではメニューのUndoを使うとキー配列差を避けられる）。単に画面上で配線が消えたことではなく、テストのsnapshot一致を合否根拠にする。

### GUIの追加確認

L06〜L09では変更後のGUI画面を保存し、テストが保存した基板を閉じて再オープンして同じ結果を確認する。スクリーンショットだけで値を判定せず、プロパティ画面・保存内容・IPC再取得を合わせる。別の破棄可能な基板Bを開き、基板Aを指定した要求がBを変えないことも確認する。拒否時は両基板の前後snapshotを比較する。通信断・readback失敗・`mutation_outcome_uncertain`では再実行せず、保存内容とlive snapshotを回収し、どこまで変更されたか記録する。

## C. v0.15.0の詳細手動評価

以下は既存のmock／fixture試験を実機で補完する追加評価であり、上のCLI CIが成功しても自動的にPASSにはならない。専用project・専用ローカルライブラリを用い、MCPクライアントの`load_toolset`と`tools/list`で現行schemaを確認してから呼び出す。同期・refreshはL08/L09と同じdry-run→`expected_plan_revision`付きapply→再計画の順で行う。各行の条件ごとに初期コピーを分ける。

| ID / Issue | 準備と操作 | 合否基準・残す証跡 | このMac |
|---|---|---|---|
| V14 / #14 | [同梱ESP32/NetTie fixture](../crates/konnect-core/tests/fixtures/placement_fidelity/README.md)を専用`.pretty`へコピー・登録し、閉じた基板と開いた基板へ`place_component`。chamfer、drill offset、margin、thermal、keepout、net-tieを比較 | 対応形状は値とgeometryの関連が保持。liveで非対応の句は名前付きで変更前に拒否されること。閉じた基板へのファイル配置の成功をlive成功に流用しない。fixture・要求・IPC結果・両方の保存データを残す | 未実施 |
| V15 / #15 | R1同期済みprojectのコピーを2組用意し、保存済みsymbol pathをroot-prefixed／root-relativeの各表現にした基板を開いて同期。別コピーで曖昧／foreign-rootも確認 | 同一部品は重複追加されず、KIID保持。曖昧／外部rootは変更前拒否。前後のpath・reference・KIID・計画を残す。[identity仕様](ISSUE_15_SYNC_IDENTITY.md) | 未実施 |
| V16 / #16 | GND銅箔zoneと別net SIG1を用意し、SIG1だけのpad-net変更を計画。別コピーでrule-areaのみ／GND側にrouteがある条件も評価 | GNDのzoneを理由に無関係なSIG1をrouted扱いしない。rule-areaも銅箔扱いしない。実routeがあるnetの制約は維持。計画とnet/zone/routing snapshotを残す。[zone仕様](PCB_ZONE_NETS.md) | 未実施 |
| V17 / #17 | 4/6/8層の専用基板と非対称のpad layer・text・chamferを持つ部品を用意し`flip_component`を往復 | In2↔In5等の対応が層数に従い、geometryとの組合せも正しい。往復で元に戻る。CLIだけでは未評価。前後の全layerと形状を保存。[mirroring仕様](implementation/issue-17-inner-layer-mirroring.md) | 未実施 |
| V18 / #18 | 2.01 mm等の十進座標で配置・移動し、回路図のlabel/textも保存。CLIでnetlist/SVGを出しGUI再オープンで照合 | 2.01 mmが2,010,000 nmとなり、保存値・応答に不要な浮動小数ノイズがない。結線も保たれる。NaN/範囲外は変更前拒否。要求・応答・保存座標・出力を残す。[座標仕様](COORDINATE_PRECISION.md) | 未実施 |
| V25 / #25 | C02に加え、BOM除外／board対象のMountingHoleをL08で同期。DNP、BOM/board/position除外を個別変更 | BOM除外だけでPCB配置から消えない。board除外と独立し、複数ユニットのnative属性とlive footprintの属性が一致。BOM・計画・回路図・PCBの属性を保存。[属性仕様](plans/issue-25-symbol-attributes.md) | 未実施 |
| V26 / #26 | 回路図にLCSC/MPN/Datasheet、PCBだけに独自fieldを追加。同期→回路図field変更/削除→再同期。別コピーでgroup化、dash図形、library refreshの`reset_text`を評価 | 所有履歴`Konnect:schematic_fields`に沿う更新・削除とPCB専用field保持、再実行noop。group等の非対応変更は無損失拒否。dashとtext表示保持、resetの明示指定時だけ表示初期化。計画・field/履歴・stroke・保存結果を残す。[field仕様](plans/issue-26-field-fidelity.md) | 未実施 |

上表V14〜V26の「このMac」列はすべて**未実施**。期待結果を実測結果として転記しない。異常系は構造化された拒否が返ることに加え、**要求前後で変更がないこと**を確認して合格とする。

## 結果の判定・記録

| 状態 | 判定 |
|---|---|
| PASS | 前提を満たし、意図したケースが実行され、必要なreadback／成果物が一致した |
| FAIL | 前提は満たすが期待と実測が異なる。再現手順と差分を保存する |
| 未実施 | まだ起動していない |
| 環境待ち | KiCad、ライブラリ、API、必要バージョン等がなく実行できない |
| 不確実 | apply後の通信断・readback不一致などで最終状態を確定できない。再適用せず調査する |

ケースごとに次を埋める。各subcaseは別行とし、件数だけでなく対応する実行名を記録する。

```text
ID / subcase:
実施日時・担当:
commit SHA / clean-or-dirty / diff:
OS / KiCad CLI・GUIのバージョン / Konnectビルド:
fixture・ライブラリの出典とバージョン / 作業コピーの絶対パス:
対象project・document・sheet・KIID / live-or-saved:
前提確認 / 実行コマンドまたはMCP要求（秘密値を除く）:
期待結果:
実測結果（テスト名、passed/failed/ignored、skipの有無）:
実行前後のsnapshot・保存ファイル・CLI成果物・ログ・画面の保存先:
判定（PASS / FAIL / 未実施 / 環境待ち / 不確実）:
差分・未確認範囲・復旧状況・再開条件:
CIの場合: run URL / attempt / headSha / job / step:
```

live snapshotは未保存状態、ファイルは保存済み状態として別名で保管する。失敗後の再試行は新しいケース番号にして、最初の失敗ログを残す。終了時はGUIを閉じて所有プロセスの終了を確認し、成果物を退避してからコピーとビルド用領域を削除する。

## 参考実績（今回のMacでの実行ではない）

2026-11-17に [run 43730504272](https://github.com/takurot/Konnect-xai/actions/runs/43730504272) のメタデータと実行ログを再取得した。実行日は2026-11-15 UTC、対象は`29a83abafe610e347eeb8516e2adc8ab0bfead8b`、Windows、CLIの実測バージョンは10.0.5。runはcompleted/success。

| 評価 | ログ上の結果 | 証明の範囲 |
|---|---|---|
| C01〜C05 | 各5テスト名の成功、合計5 passed / 0 failed / 0 ignored。native assembly属性テストも実行 | そのSHA・Windows・CLI 10.0.5のファイル／MCP経路 |
| C06 | 4 passed / 0 failed / 0 ignored、`parsed 115/115 demo schematics` | 同梱デモに対するparse等。GUI評価ではない |
| C07 | 対象variantテスト名が成功、1 passed / 0 failed / 0 ignored。CLI探索skipなし | 実CLIによるvariant評価 |
| C08 | 1 passed / 0 failed / 0 ignored、30.02秒 | mock応答停止のタイムアウト |
| C09 | `passed PCM validation` | Windows ZIPのvalidator検証。GUIインストールは未評価 |
| L01〜L10・V14〜V26の手動部分 | このrunでは実施していない | GUI/live IPCの合格実績として扱わない |

本書のコマンド・関数名・fixtureパスは上記ソースと照合した。KiCad導入後は、この表を流用せず新しいSHA・環境の結果を記録する。
