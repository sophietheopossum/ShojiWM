---
sidebar_position: 7
---

# ウィンドウの合成

`COMPOSITOR.window.composition` は ShojiWM のカスタマイズの心臓部です。ウィンドウを
受け取り、それをどう配置・装飾するかを表す TSX ツリーを返す関数を割り当てます。
コンポジターはすべてのトップレベルウィンドウに対してこれを呼び、読み取った値が
変化するたびに（差分的に）再実行します。

```tsx
COMPOSITOR.window.composition = (window) => (
  <ManagedWindow rect={window.position} zIndex={1}>
    <WindowBorder
      style={{borderRadius: 10, border: {px: 2, color: window.isFocused((f) => (f ? '#d7ba7d' : '#4f5666'))}}}
    >
      <Box direction="column">
        <Box direction="row" style={{height: 28, paddingX: 8, gap: 8, alignItems: 'center'}}>
          <AppIcon icon={window.icon} style={{width: 16, height: 16}} />
          <Label text={window.title} style={{flexGrow: 1, fontSize: 13}} />
        </Box>
        <ClientWindow />
      </Box>
    </WindowBorder>
  </ManagedWindow>
);
```

ツリーには必ず、ちょうど1つの [`<ManagedWindow/>`](#managedwindow) が、ちょうど1つの
[`<ClientWindow/>`](#clientwindow) を包む形で含まれている必要があります。その間にある
もの――ボーダー・タイトルバー・ボタン――があなたの装飾で、[SSD
コンポーネント](./components.md) で組み立てます。

## `window` オブジェクト

引数は `WaylandWindow`、つまり1つのウィンドウへのライブでリアクティブなハンドルです。
合成内でそのシグナルを読むと、変更に自動的に購読されます。

### リアクティブなプロパティ

それぞれ `ReadonlySignal` です――`window.title()` や `window.title.value` のように
読むか、`window.isFocused((f) => f ? 'a' : 'b')` のようにマップします。

| プロパティ | 型 | 意味 |
| --- | --- | --- |
| `title` | `string` | ウィンドウタイトル |
| `appId` | `string \| undefined` | アプリケーション id（例: `"org.gnome.Nautilus"`） |
| `icon` | `WindowIcon \| undefined` | アプリケーションアイコン |
| `isFocused` | `boolean` | キーボードフォーカスを持つ |
| `isFloating` | `boolean` | フローティング（非タイル） |
| `isMaximized` | `boolean` | 最大化 |
| `isFullscreen` | `boolean` | フルスクリーン |
| `isResizable` | `boolean` | クライアントがインタラクティブリサイズを許可 |
| `isTransient` | `boolean` | 別ウィンドウの子（ダイアログ） |
| `parentId` | `string \| undefined` | トランジェントの場合の親ウィンドウ id |
| `sizeConstraints` | `WindowSizeConstraints` | クライアントの最小／最大サイズ |
| `interaction` | スナップショット | 現在のポインター／ドラッグの状態 |

非リアクティブなヘルパー: `id`（安定した文字列）、`position` / `rect`（現在の論理
ジオメトリ）、`state`（ウィンドウごとのストア。[状態とシグナル](./state-and-signals.md)
を参照）、`transform`（GPU トランスフォーム）、`animation`
（[アニメーション](./animations.md) を参照）。

### メソッド

| メソッド | 効果 |
| --- | --- |
| `close()` | クライアントに閉じるよう要求 |
| `maximize()` / `unmaximize()` | 最大化の切り替え |
| `minimize()` | 最小化 |
| `fullscreen()` / `unfullscreen()` | フルスクリーンの切り替え |
| `focus()` | キーボードフォーカスを与え前面に出す |
| `scheduleAnimation(options)` | マネージドウィンドウのジオメトリをアニメーション |
| `cancelAnimation(channel?)` | 実行中のアニメーションをキャンセル |
| `setCloseAnimationDuration(ms)` | 閉じるアニメーションに合わせてサーフェス破棄を遅延 |
| `isXWayland()` | XWayland 上で動作中なら `true` |

## ManagedWindow

`<ManagedWindow/>` はウィンドウをレイアウトシステムに結びつけるアンカーです。
ウィンドウごとに1つ置きます。

| Prop | 型 | 意味 |
| --- | --- | --- |
| `rect` | `ManagedWindowRect` | ウィンドウの論理的な `{x, y, width, height}` |
| `zIndex` | `number` | 重なり順（大きいほど上） |
| `workspace` | `string \| number` | ワークスペース割り当て |
| `visibleOutputs` | `string[] \| null` | 指定出力に限定（`null` で全出力） |
| `visible` | `boolean` | アンマップせずに表示／非表示 |
| `idle` | `boolean` | フォーカス巡回から除外。背景として扱う |
| `interactive` | `boolean` | `false` のときポインター入力を無視 |
| `forceRectSize` | `boolean` | クライアントを `rect` のサイズに強制 |
| `tiled` | `boolean` | タイル状態をクライアントに送る |
| `opacity` | `number` | `0.0`〜`1.0` |
| `transform` | `ManagedWindowTransform` | 追加の GPU トランスフォーム |
| `allowTearing` | `boolean` | フルスクリーン＋ダイレクトスキャンアウト時のテアリングを許可（ゲーム向け） |

すべての prop はリアクティブなレイアウトのためにシグナルを受け付けます。`rect`・
`zIndex` などは通常、あなたのウィンドウマネージャのロジックが駆動します。

## ClientWindow

`<ClientWindow/>` はクライアントの実際のサーフェスバッファを描画します。リーフノードで
子要素は持ちません。別名: `<Window/>`。

```tsx
<ClientWindow style={{borderRadius: 8}} />
```

任意の `style` はサーフェスをクリップ／装飾します（通常は `borderRadius` のみ）。

:::tip フルスクリーンのファストパス
フルスクリーンのウィンドウでは、`<ManagedWindow/>` の中に**素の `<ClientWindow/>`
だけ**を返します（ボーダーもタイトルバーもなし）。他に何も描画しないことで、TTY
バックエンドがクライアントバッファをプライマリプレーンに昇格（ダイレクトスキャンアウト）
でき、最小のレイテンシになります。デフォルト設定はまさにこれを行っています。
:::
