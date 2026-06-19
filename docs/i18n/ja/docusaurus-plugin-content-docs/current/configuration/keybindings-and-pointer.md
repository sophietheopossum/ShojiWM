---
sidebar_position: 5
---

# キーバインドとポインター

## キーボードショートカット

`COMPOSITOR.key.bind(id, shortcut, handler, options?)` はコンポジターレベルの
キーボードショートカットを登録します。

```ts
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});
```

| 引数 | 型 | 意味 |
| --- | --- | --- |
| `id` | `string` | 一意な名前（ヘルプ UI に表示）。同じ id で再登録すると上書き。 |
| `shortcut` | `string` | モディファイア＋キーの記法（例: `"Super+Shift+Left"`） |
| `handler` | `() => void` | ショートカット発火時に呼ばれる |
| `options` | `{on?: "press" \| "release"}` | 発火タイミング。デフォルトは `"press"` |

### ショートカットの記法

モディファイアとキーを `+` で組み合わせます。

- **モディファイア:** `Super`・`Ctrl`・`Shift`・`Alt`
- **キー:** 英字（`T`・`Q`）、矢印（`Left`・`Right`・`Up`・`Down`）、
  ファンクションキー（`F`）など

```ts
COMPOSITOR.key.bind('close', 'Super+Q', () => focused?.close());
COMPOSITOR.key.bind('move-tile-left', 'Super+Shift+Left', () => moveTile(-1));
COMPOSITOR.key.bind('screenshot', 'Super+P', () => {
  COMPOSITOR.process.spawn({command: 'hyprshot -m region --raw | swappy -f -'});
});
```

### タップバインド（`on: "release"`）

モディファイア単体を `{on: "release"}` で登録すると **タップ** になります。キーを
離したときに発火しますが、その間に他のキーやボタンが押されていない場合に限ります。
デフォルト設定では、`Super` を素早くタップするとランチャーを開きつつ、`Super` を
他のショートカットのモディファイアとしても使えるようにこれを利用しています。

```ts
COMPOSITOR.key.bind('launcher-tap', 'Super', openLauncher, {on: 'release'});
```

## ポインター

`COMPOSITOR.pointer` は、コンポジター自身が扱うマウス操作を設定します。（加速度・
スクロール方式などデバイスごとの調整は [`COMPOSITOR.input`](./input.md) を使います。）

### モディファイアでウィンドウを移動

`bindWindowMoveModifier(modifier)` を使うと、モディファイアを押しながらウィンドウ上の
どこをクリックしてもドラッグで移動できます――タイトルバーをつかむ必要はありません。

```ts
COMPOSITOR.pointer.bindWindowMoveModifier('Super');
```

:::tip
インタラクティブなリサイズの当たり判定は、ウィンドウごとに
`<WindowBorder interaction={{resizeHitArea: …}}>` の prop で設定します。
[SSD コンポーネント](./components.md#windowborder) を参照してください。
:::
