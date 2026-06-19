---
sidebar_position: 1
---

# インストール

## 前提条件

- 動作する Wayland/DRM 環境を備えた Linux システム
- 最近の Rust ツールチェーン
- Node.js 18 以降

## ビルド

```bash
git clone https://github.com/bea4dev/ShojiWM.git
cd ShojiWM
cargo build --release
```

## 実行

```bash
./target/release/shoji_wm
```
