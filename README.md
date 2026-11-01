# reusemic

[![CI](https://github.com/tsuzu/reusemic/actions/workflows/ci.yml/badge.svg)](https://github.com/tsuzu/reusemic/actions/workflows/ci.yml)

NVIDIA の RE-USE 系音声強調モデルでマイク音声をリアルタイムにノイズ除去し、macOS の仮想マイク **「ReuseMic」** として提供します。Zoom / Meet / Discord / OBS などで入力デバイスに「ReuseMic」を選ぶだけで使えます。

- BlackHole などの外部ツールは不要です(仮想デバイスは同梱の HAL プラグインが提供します)。
- Python は使いません。ドライバは C、アプリは Rust です。推論は [reuse-rs](https://github.com/altunenes/re-use-rs) を使い、CPU 上で Accelerate を使って動きます。
- 実行時に必要なのは、アプリのバイナリ、ドライバ、重みファイルだけです。

## モデルについて

[nvidia/RE-USE](https://huggingface.co/nvidia/RE-USE) は双方向 Mamba のオフラインモデルで、数秒単位でまとめて処理する前提になっています。そのため、リアルタイムのマイクには同じ系列のストリーミング版 [nvidia/Real-time_RE-USE](https://huggingface.co/nvidia/Real-time_RE-USE)(因果的、20 ms フレーム)を使います。

> 重みのライセンスは NVIDIA One-Way Noncommercial License(研究・非商用のみ)です。

## 仕組み

```
物理マイク ──IOProc──▶ [reusemic]
                         48k→16k リサンプル → Real-time RE-USE → 16k→48k(クロックずれ補正付き)
                                   │
                                   ▼ IOProc (出力)
                 ┌──────── ReuseMic.driver (HAL プラグイン) ────────┐
                 │  "ReuseMic Feed"(非表示の出力) ─ring buffer─▶ "ReuseMic"(入力) │
                 └────────────────────────────────────────────────────┘
                                   │
                                   ▼
                          Zoom / Meet / Discord …
```

- 2 つのデバイスは同じクロックとリングバッファを共有しています。フィードにサンプル時刻 T で書いた音は、マイク側の時刻 T でそのまま読めます。
- reusemic が止まっている間、ReuseMic は無音になります。
- 物理マイクと仮想デバイスのクロックのずれは、出力側リサンプラの比率をわずかに調整して吸収します(最大 ±0.5%)。

## 必要なもの

- macOS 11 以降(Apple Silicon 推奨)
- Xcode Command Line Tools(`clang`、`codesign`)
- Rust(`cargo`)

## ビルドとインストール

```sh
make            # ドライバとアプリをビルド
make model      # 重み(約 54 MB)を models/ にダウンロード
make test       # ドライバとユニットのテスト
make install    # ドライバを /Library/Audio/Plug-Ins/HAL に入れて coreaudiod を再起動(sudo)
```

## 使い方

```sh
./target/release/reusemic --list                 # デバイス一覧
./target/release/reusemic                        # 既定の入力デバイスをノイズ除去して ReuseMic に出す
./target/release/reusemic --input "MacBook Pro"  # マイクを指定
```

あとは通話アプリの入力デバイスに **ReuseMic** を選んでください。初回起動時に、ターミナルへのマイク許可を求められます。

実行中の操作:`b` + Enter でバイパス(素の音声との聴き比べ)の切り替え、`q` + Enter で終了します。

| オプション | 既定値 | 説明 |
|---|---|---|
| `--layers 3-12` | 4 | Mamba の層数。多いほど高品質で重くなります |
| `--look-ahead 0-2` | 0 | 先読みするフレーム数。1 フレームにつき遅延が 20 ms 増えます |
| `--rate` | 16000 | モデルを動かすサンプルレート。48000 にすると CPU 負荷が約 3 倍になります |
| `--buffer ms` | 60 | 出力側のジッタバッファ。アンダーランが出るなら増やしてください |
| `--channel n` | 0 | 複数チャンネルのマイクで使うチャンネル |
| `--model path` | 自動 | 重みファイルのパス |
| `--file in.wav out.wav` | | ライブと同じ処理をファイルにかけて確認します |
| `--probe` | | ReuseMic に届いている音量を表示します |
| `--probe-wav out.wav` | | 同上、届いた音声を wav に保存します |
| `--test-tone` | | マイクの代わりに 440 Hz の音を流します(経路の確認用。モデルを通すと消えるので `--bypass` と併用) |

ステータス行の `model xx ms/frame` が 20 ms を下回っていれば、リアルタイムに間に合っています。遅延の目安は「バッファ + 20 ms + 先読み + デバイスバッファ」で、既定値ならおよそ 90 ms です。

## アンインストール

```sh
make uninstall
```

## ライセンス

このリポジトリのコードは [MIT](LICENSE-MIT) または [Apache-2.0](LICENSE-APACHE) のどちらかを選んで利用できるデュアルライセンスです。

モデルの重み([nvidia/Real-time_RE-USE](https://huggingface.co/nvidia/Real-time_RE-USE))はこのリポジトリに含めていません。`make model` で NVIDIA の配布元から取得し、**NVIDIA One-Way Noncommercial License (NSCLv1)** の下で利用します(研究・非商用のみ)。重みを使った成果物にはこのライセンス条件がかかるので注意してください。
