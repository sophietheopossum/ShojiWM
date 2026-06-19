---
sidebar_position: 9
---

# 状態とシグナル

ShojiWM の合成は**リアクティブ**です。コードが読んだ値が変化すると、装飾の影響を
受けた部分が自動的に再評価されます。その土台となるプリミティブが**シグナル**です。

## シグナル

シグナルは値の入れ物です。合成内で読むとその合成が変更に購読され、書き込むと購読者へ
通知されます。

### 読む

`ReadonlySignal<T>` は3通りの方法で読め、派生シグナルにマップできます。

```ts
window.title.value          // 値を読む
window.title()              // 同じ（呼び出し形式）
window.title.peek()         // 購読せずに読む
window.isFocused((f) => f ? '#d7ba7d' : '#4f5666')  // マップした派生シグナルを作る
```

マップ形式は TSX 内の主役です――手動の配線なしに新しいリアクティブな値を生み出します。

```tsx
<Label text={window.title} style={{color: window.isFocused((f) => (f ? '#fff' : '#aaa'))}} />
```

### 書く

書き込み可能な `Signal<T>` には `.value =`・`.set(...)`・`.update(...)` が加わります。

```ts
count.value = 5;
setCount(5);              // [count, setCount] として分解したとき
count.update((n) => n + 1);
```

## モジュールスコープのヘルパー

設定のトップレベル（コンポーネント関数の外）で使います。

| 関数 | 目的 |
| --- | --- |
| `signal(initial)` | 書き込み可能なシグナルを作成。`[signal, setter]` として分解可。 |
| `computed(fn)` | 派生した読み取り専用シグナルを作成。依存が変わると再計算。 |
| `effect(fn)` | 依存が変わるとサイドエフェクトを実行。破棄関数を返す。 |
| `read(maybeSignal)` | 値かシグナルかを素の値に展開。 |
| `isSignal(x)` | `unknown` をシグナルに絞り込む。 |

```ts
import {signal, computed, effect} from 'shoji_wm';

const [count, setCount] = signal(0);
const doubled = computed(() => count.value * 2);
const dispose = effect(() => console.log('count is', count.value));
setCount(1); // "count is 1" を出力
```

## コンポーネントスコープのフック

自分で定義した関数コンポーネント（TSX コンポーネント）の中では、フック形式を使います。
React のフックのように、再レンダリングをまたいで安定したアイデンティティを保ちます。

| フック | 目的 |
| --- | --- |
| `useState(initial)` | コンポーネントローカルな書き込み可能シグナル（`[signal, setter]`） |
| `useComputed(fn)` | コンポーネントローカルな派生シグナル |
| `useEffect(fn, deps?)` | レンダリング後のサイドエフェクト。クリーンアップを返せる |
| `useLayoutEffect(fn, deps?)` | `useEffect` と同様だがレンダリングパス中に同期実行 |
| `useMemo(fn, deps?)` | 素の（シグナルでない）値をメモ化 |
| `useRef(initial)` | 再レンダリングをまたいで保持されるミュータブルな `.current` |
| `onCleanup(fn)` | コンポーネントのアンマウント時の後始末を登録 |

```tsx
const CloseButton = ({window}: {window: WaylandWindow}) => {
  const [hover, setHover] = useState(false);
  return (
    <Button
      onHoverChange={setHover}
      onClick={window.close}
      style={{background: hover((h) => (h ? '#FFFFFF40' : '#FFFFFF20'))}}
    />
  );
};
```

## ウィンドウごとの状態

`createWindowState` は、各ウィンドウにスコープされた名前付きのリアクティブな状態スロットを
宣言します。モジュールスコープで一度呼んでキーを取得し、合成やイベントハンドラの中で
`window.state[key]`（`Signal<T>`）を読みます。

```ts
import {createWindowState} from 'shoji_wm';

// モジュールスコープ — キーを一度だけ作成
const isMinimized = createWindowState('minimized', {default: false});

// 合成内で読む（リアクティブ）
COMPOSITOR.window.composition = (window) => {
  const minimized = window.state[isMinimized]; // Signal<boolean>
  return <ManagedWindow visible={minimized((v) => !v)} /* … */ />;
};

// イベントハンドラで書く
COMPOSITOR.event.onFocus((window) => {
  window.state[isMinimized].set(false);
});
```

`default` には値、またはウィンドウ依存の初期状態のためのファクトリー
`(window) => value` を渡せます。デフォルト設定は、タイル状態・ワークスペースの表示・
フルスクリーン・アニメーションのオフセットを追跡するためにウィンドウごとの状態を
多用しています。
