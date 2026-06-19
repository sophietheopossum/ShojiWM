---
sidebar_position: 10
---

# エフェクト

ShojiWM は GPU シェーダーエフェクトを4箇所で実行でき、`COMPOSITOR.effect` で設定します。

| フィールド | 型 | 適用先 |
| --- | --- | --- |
| `background_effect` | `CompiledEffectHandle \| null` | 全ウィンドウの下のフルスクリーン背景 |
| `window` | `(window) => WindowEffectAssignment \| null` | トップレベルウィンドウごと |
| `layer` | `(layer) => LayerEffectAssignment \| null` | レイヤーシェルサーフェスごと（バー・ドック） |
| `popup` | `(popup) => PopupEffectAssignment \| null` | ポップアップごと（メニュー・ツールチップ） |

合成内の領域にエフェクトを適用することもできます
（[`<ShaderEffect/>`](./components.md#shadereffect)）。

## 背景エフェクト

すべての背後に描画されるコンパイル済みエフェクトを割り当てます。`null` で無効化。

```ts
import {COMPOSITOR, compileEffect, backdropSource, dualKawaseBlur} from 'shoji_wm';

COMPOSITOR.effect.background_effect = compileEffect({
  input: backdropSource(),
  invalidate: {kind: 'on-source-damage-box', antiArtifactMargin: 8},
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});
```

## ウィンドウ／レイヤー／ポップアップごとのエフェクト

各ファクトリーはサーフェスごとに呼ばれ、割り当てを返すか、エフェクト無しなら
`null`／`{}` を返します。レイヤーとポップアップの割り当ては `behind` を使って
サーフェスの背後にエフェクトを描画します（デフォルト設定はバーやメニューの背後を
すべてぼかします）。

```ts
const LAYER_BLUR = compileLayerEffect({
  input: backdropSource(),
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

COMPOSITOR.effect.layer = (layer) => {
  if (layer.namespace() === 'no_blur') return {};
  return {behind: LAYER_BLUR};
};

COMPOSITOR.effect.popup = (popup) => {
  if (popup.parentKind === 'window') return {};
  return {behind: POPUP_BLUR};
};
```

## エフェクトを組み立てる

エフェクトは **ソース入力＋ステージのパイプライン**です。使う場所に応じたコンパイル
関数でコンパイルします。

| コンパイラ | 生成物 | 用途 |
| --- | --- | --- |
| `compileEffect(opts)` | `CompiledEffectHandle` | 背景・`<ShaderEffect/>` |
| `compileWindowEffect(opts)` | `WindowEffectHandle` | `COMPOSITOR.effect.window` |
| `compileLayerEffect(opts)` | `LayerEffectHandle` | `COMPOSITOR.effect.layer` |
| `compilePopupEffect(opts)` | `PopupEffectHandle` | `COMPOSITOR.effect.popup` |

オプション:

| オプション | 型 | 意味 |
| --- | --- | --- |
| `input` | ソースハンドル | パイプラインが読む対象（例: `backdropSource()`） |
| `pipeline` | ステージ配列 | 順に適用されるステージ |
| `invalidate` | ポリシー | 再描画のタイミング（下記参照） |
| `alpha` | `"opaque" \| "preserve"` | 透明度を表示まで維持（デフォルト `"opaque"`） |
| `outsets` | `EffectOutsets` | （ウィンドウエフェクト）ウィンドウ境界の外側に描画 |

### ソース

| ソース | 読み取る対象 |
| --- | --- |
| `backdropSource()` | 対象の背後に合成済みのシーン |
| `windowSource()` | ウィンドウ自身のサーフェス |
| `layerSource()` | レイヤーサーフェス自身の内容 |
| `popupSource()` | ポップアップ自身の内容 |
| `imageSource(path)` | 静的な画像ファイル |

### ステージ

| ステージ | 目的 |
| --- | --- |
| `dualKawaseBlur({radius, passes})` | 高速で広いブラー |
| `shaderStage(shader, {uniforms, textures})` | カスタム GLSL フラグメントシェーダーを実行 |
| `noise({...})` | フィルムグレイン風のノイズを追加 |
| `save(name)` / `blend(input, {...})` | 中間結果の保存／合成 |

`shaderStage` はシェーダー（パス、または `loadShader(path)` ハンドル）に加えて、
`uniforms`（シェーダーに渡す数値・色）と `textures`（名前で束縛する追加のソース
ハンドル）を取ります。

```ts
import {compileEffect, backdropSource, dualKawaseBlur, shaderStage, loadShader} from 'shoji_wm';

const liquidGlass = compileEffect({
  input: backdropSource(),
  invalidate: {kind: 'on-source-damage-box', antiArtifactMargin: 8},
  pipeline: [
    dualKawaseBlur({radius: 4, passes: 2}),
    shaderStage(loadShader('./src/liquid-glass.frag'), {
      uniforms: {
        glass_radius_px: 10.0,
        distortion_strength: 0.15,
        chromatic_shift_px: 3.0,
      },
    }),
  ],
});
```

### 無効化ポリシー

`invalidate` はエフェクトの再描画タイミングを制御し、新鮮さとコストのバランスを取ります。

- `{kind: 'on-source-damage-box', antiArtifactMargin: N}` — 変化した領域だけを、エッジの
  アーティファクトを避けるため `N` px 広げて再描画。通常はこれを選びます。
- `'always'` — 毎フレーム再描画（高コスト。アニメーションするシェーダー向け）。
- 自分で無効化を行う手動ポリシー。

### アルファ

パイプラインの出力が透明であるべき場合（例: レイヤー自身のアルファマスクでクリップした
ブラー）は `alpha: 'preserve'` を設定します。これにより透明度が不透明に強制されず、
表示パスまで維持されます。
