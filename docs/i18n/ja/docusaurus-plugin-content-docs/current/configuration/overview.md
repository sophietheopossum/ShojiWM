---
sidebar_position: 1
---

# 概要

ShojiWM の設定はすべて TypeScript/TSX で書きます。設定ファイルは `shoji_wm` から
import する普通の TypeScript モジュールで、コンポジターとのやり取りはただ一つの
ルートオブジェクト **`COMPOSITOR`** を通じて行います。

```tsx
import {COMPOSITOR, ManagedWindow, ClientWindow, WindowBorder} from 'shoji_wm';

// Super+T でターミナルを起動
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});

// すべてのウィンドウの装飾方法を決める
COMPOSITOR.window.composition = (window) => (
  <ManagedWindow rect={window.position} zIndex={1}>
    <WindowBorder style={{borderRadius: 10, border: {px: 2, color: '#d7ba7d'}}}>
      <ClientWindow />
    </WindowBorder>
  </ManagedWindow>
);
```

:::tip
設定はデフォルトで `~/shoji_wm/config/` を読みます。このセクションの例の多くは
デフォルト設定（`packages/config/src/index.tsx`）から引用しています。下記の各機能を
理解した後は、このファイルが最良の総合リファレンスになります。
:::

## `COMPOSITOR` オブジェクト

`COMPOSITOR` は設定可能な領域を名前付きフィールドにまとめたものです。各フィールドは
このセクションの個別ページに対応します。

| フィールド | 制御する内容 | ページ |
| --- | --- | --- |
| `event` / `onEnable` / `onDisable` | ライフサイクルフックとウィンドウ・入力・出力のイベントバス | [ライフサイクルとイベント](./lifecycle-and-events.md) |
| `output` | モニターの解像度・スケール・位置・ミラーリング | [出力（ディスプレイ）](./outputs.md) |
| `input` | キーボード・ポインター・タッチパッドのデバイス設定 | [入力デバイス](./input.md) |
| `key` / `pointer` | キーボードショートカットとポインターのモディファイア | [キーバインドとポインター](./keybindings-and-pointer.md) |
| `process` / `env` | プログラムの起動と環境変数 | [プロセスと環境変数](./processes-and-env.md) |
| `window` | ウィンドウごとの装飾（合成関数） | [ウィンドウの合成](./window-composition.md) |
| `effect` | GPU エフェクト：背景ブラー、ウィンドウ／レイヤー／ポップアップごとのシェーダー | [エフェクト](./effects.md) |
| `debug` | FPS カウンターなどのデバッグオーバーレイ | [ライフサイクルとイベント](./lifecycle-and-events.md) |

合成関数の内側で使う部品にも、それぞれページがあります。

- [SSD コンポーネント](./components.md) — `<Box/>`・`<Label/>`・`<Button/>`・
  `<AppIcon/>`・`<Image/>`・`<ShaderEffect/>`・`<WindowBorder/>`・
  `<ManagedWindow/>`・`<ClientWindow/>` と `style` の全リファレンス。
- [状態とシグナル](./state-and-signals.md) — 自動更新を支えるリアクティブモデル。
- [アニメーション](./animations.md) — なめらかなトランジションの作り方。

## 全体のメンタルモデル

1. **コンポジターのコア（Rust）** が、各ウィンドウ・出力・入力デバイスの
   ライブでリアクティブなビューを設定側に送ります。
2. 設定はその値を読み、コールバックを登録し、（ウィンドウについては）装飾を表す
   小さな **TSX ツリー**を返します。
3. 読み取った値のどれかが変化すると、影響を受けた部分だけが再評価されます。これが
   **シグナル**システムです（[状態とシグナル](./state-and-signals.md) を参照）。

まだの場合は、両者がどう噛み合うかを
[アーキテクチャ概要](../architecture/shojiwm.md) で先に読んでおくと理解が深まります。
