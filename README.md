# akg

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Termux-green.svg)](https://termux.dev/)
[![Version](https://img.shields.io/badge/version-0.5.1-blue.svg)](https://github.com/lev73748/akg)

Fast package manager for Termux written in Rust.  
Compatible with the apt/dpkg package format and repository layout.

---

## Features

- Parallel package downloads
- Dependency resolution (Depends, Pre-Depends, Conflicts, Breaks, Provides, Replaces)
- Local `.deb` installation (file path or URL)
- Custom repositories via `--repo`
- Maintainer scripts and dpkg-style triggers
- Auto-installed marks (apt-compatible)
- Atomic status database updates
- Dry-run and download-only modes
- Optional root via `AKG_ROOT`

---

## Installation

### From source

```bash
git clone https://github.com/lev73748/akg.git
cd akg
cargo build --release
cp target/release/akg $PREFIX/bin/
```

Requirements: Rust toolchain, `gpgv` (for repository signature verification).

---

## Usage

```
akg [options] <command> [args]
akg -f <file.deb>
```

### Commands

| Command            | Aliases          | Description                          |
|--------------------|------------------|--------------------------------------|
| `update`           | `upd`            | Refresh package index                |
| `install`          | `i`, `add`       | Install packages or local `.deb`     |
| `reinstall`        | `rei`            | Reinstall packages                   |
| `upgrade`          | `up`, `upg`      | Upgrade all packages                 |
| `uninstall`        | `remove`, `rm`   | Remove packages + unused deps        |
| `purge`            |                  | Remove including config files        |
| `autoremove`       |                  | Remove unused dependencies           |
| `search`           | `se`             | Search packages                      |
| `show`             | `sh`, `info`     | Show package details                 |
| `files`            | `f`              | List files of an installed package   |
| `list-installed`   | `list-i`         | List installed packages              |
| `clean`            |                  | Delete cached `.deb` files           |

### Options

| Option                  | Description                                      |
|-------------------------|--------------------------------------------------|
| `-y`, `--yes`           | Assume yes                                       |
| `-j`, `--jobs N`        | Number of parallel downloads                     |
| `-f`, `--file PATH`     | Install local `.deb` (path or URL), repeatable   |
| `-r`, `--repo URL`      | Use custom repository instead of sources.list    |
| `--suite NAME`          | Repository suite (default: `stable`)             |
| `--comp NAME`           | Repository component (default: `main`)           |
| `--no-verify`           | Skip Release signature check                     |
| `--download-only`       | Only download `.deb` files                       |
| `--dry-run`             | Show the plan without making changes             |
| `--no-scripts`          | Do not run maintainer scripts                    |
| `--no-triggers`         | Do not run triggers                              |
| `--force-overwrite`     | Allow overwriting files of other packages        |
| `-V`, `--version`       | Print version                                    |
| `-h`, `--help`          | Show help                                        |

### Examples

```bash
# Update index
akg update

# Install packages
akg install neovim htop

# Install local .deb
akg -f ./package.deb
akg install https://example.com/package.deb

# Use a custom mirror
akg -r mirror.yandex.ru/mirrors/termux/termux-main install fish

# Upgrade everything
akg upgrade

# Remove package and unused dependencies
akg uninstall nodejs

# Search
akg search python
```

### Environment

| Variable     | Description                                      |
|--------------|--------------------------------------------------|
| `PREFIX`     | Termux prefix (default: `/data/data/com.termux/files/usr`) |
| `AKG_ROOT`   | Optional root for testing (e.g. `$HOME/fakeroot`) |
| `AKG_ARCH`   | Force architecture                               |

Test mode example:

```bash
AKG_ROOT=$HOME/fakeroot akg install -y --no-scripts <pkg>
```

---

## Building

```bash
cargo build --release
```

Release profile uses fat LTO, single codegen unit and binary stripping.

---

## License

MIT License. See [LICENSE](LICENSE).

---

## На русском

Быстрый пакетный менеджер для Termux, написанный на Rust.  
Совместим с форматом пакетов и репозиториев apt/dpkg.

### Возможности

- Параллельная загрузка пакетов
- Разрешение зависимостей (Depends, Pre-Depends, Conflicts, Breaks, Provides, Replaces)
- Установка локальных `.deb` (путь или URL)
- Свои репозитории через `--repo`
- Maintainer-скрипты и триггеры в стиле dpkg
- Метки auto-installed (совместимо с apt)
- Атомарные обновления базы status
- Режимы dry-run и download-only
- Тестовый root через `AKG_ROOT`

### Установка из исходников

```bash
git clone https://github.com/lev73748/akg.git
cd akg
cargo build --release
cp target/release/akg $PREFIX/bin/
```

Нужен toolchain Rust и `gpgv` (для проверки подписей репозиториев).

### Основные команды

| Команда            | Алиасы           | Описание                             |
|--------------------|------------------|--------------------------------------|
| `update`           | `upd`            | Обновить индекс                      |
| `install`          | `i`, `add`       | Установить пакеты или локальный `.deb` |
| `reinstall`        | `rei`            | Переустановить                       |
| `upgrade`          | `up`, `upg`      | Обновить все пакеты                  |
| `uninstall`        | `remove`, `rm`   | Удалить + неиспользуемые зависимости |
| `purge`            |                  | Удалить вместе с конфигами           |
| `autoremove`       |                  | Удалить неиспользуемые зависимости   |
| `search`           | `se`             | Поиск пакетов                        |
| `show`             | `sh`, `info`     | Информация о пакете                  |
| `files`            | `f`              | Список файлов установленного пакета  |
| `list-installed`   | `list-i`         | Список установленных пакетов         |
| `clean`            |                  | Очистить кэш `.deb`                  |

### Примеры

```bash
akg update
akg install neovim htop
akg -f ./package.deb
akg -r mirror.yandex.ru/mirrors/termux/termux-main install fish
akg upgrade
akg uninstall nodejs
akg search python
```

### Переменные окружения

| Переменная   | Описание                                         |
|--------------|--------------------------------------------------|
| `PREFIX`     | Префикс Termux (по умолчанию `/data/data/com.termux/files/usr`) |
| `AKG_ROOT`   | Корневой каталог для тестов                      |
| `AKG_ARCH`   | Принудительная архитектура                       |

### Лицензия

MIT. См. [LICENSE](LICENSE).
```
