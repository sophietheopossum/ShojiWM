---
sidebar_position: 1
---

# Waylandとは
Waylandとはディスプレイプロトコルの名称です。従来はX Window Systemが主流でした。
しかし近年ではXよりもWaylandのほうが活発に開発されています。
Xともっとも異なるところとしては、セキュリティの向上・パフォーマンスの向上が挙げられるでしょう。

## アーキテクチャ
従来のXは以下のようになっていました。

![X11](https://wayland.freedesktop.org/x-architecture.png)

X serverがクライアントとやり取りし、コンポジターとの仲介役となります。
ここでのコンポジターとはMutter(GNOME)やKWin(KDE Plasma)といったウィンドウの管理等を行っているプログラムのことを指します。

X Window Systemのアーキテクチャには以下のような問題が存在します。

- セキュリティモデルが古い
- ほとんどの操作においてX serverが仲介してしまいパフォーマンスが下がる
- コンポジターが後付け
- 1.5倍や1.75倍といった分数スケーリングに対応していない
- プロトコルが巨大で実装が難しい

対してWaylandはこれらの問題を解決するために以下のようなシンプルなアーキテクチャを採用しています。

![Wayland](https://wayland.freedesktop.org/wayland-architecture.png)

Waylandコンポジターが直接クライアントとやり取りすることで無駄なバッファコピーが削減され、
直接KMS / kernelとやり取りするのでパフォーマンスが向上します。
また、分数スケーリング等の新しいプロトコルもサポートします。

## 用語
ここではWaylandにおけるいくつかの用語を解説します。

### DRM / KMS
Linux で GPU やディスプレイ出力を扱うためのカーネル側の仕組みです。
DRM は Direct Rendering Manager の略です。カーネルからGPUを安全に扱うための仕組みです。
KMS は Kernel Mode Setting の略です。ディスプレイ設定を扱います。

### page flip
page flip は、表示中の画像を別の画像に切り替えることです。
画面の1フレームの内容が描かれているframebufferを入れかえることで、
描画途中に画面に描かずに、1フレームすべての描画が終わってからフレームの参照先を変更します。

```
1. アプリや compositor が次の画像を作る
2. その画像を framebuffer に入れる
3. DRM/KMS に「次はこれを表示して」と依頼する
4. ディスプレイ表示が切り替わる
```
こうすることでウィンドウの描画途中に表示されてしまうのを防ぎます。

### Direct Scanout
Direct Scanout は、Wayland compositor がアプリの buffer を合成せず、そのままディスプレイに出す最適化です。
例えばウィンドウ1が他のすべての要素を覆った状態、つまりフルスクリーン状態であればKMS planeに直接載せて他の処理をスキップできます。
こうすることでGPUの負荷や遅延を減らします。
