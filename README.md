# akg

[

![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)

](LICENSE)
[

![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange.svg)

](https://www.rust-lang.org/)
[

![Platform](https://img.shields.io/badge/Platform-Termux-green.svg)

](https://termux.dev/)
[

![Version](https://img.shields.io/badge/version-0.5.3-blue.svg)

](https://github.com/lev73748/akg)

Fast package manager for Termux written in Rust.
Compatible with the apt/dpkg package format and repository layout.

---

## Features

- Parallel package downloads and unpacking
- Progress bars: speed, ETA, packages done, time of every phase
- Dependency resolution (Depends, Pre-Depends, Conflicts, Breaks, Provides, Replaces)
- Local `.deb` installation (file path or URL), no download needed for files
- Custom repositories via `--repo` (just an address, no `sources.list` editing)
- Repository signature check and SHA-256 check of indexes and packages
- Maintainer scripts and dpkg-style triggers
- Auto-installed marks (apt-compatible)
- Atomic status database updates
- Dry-run and download-only modes
- Optional root via `AKG_ROOT`

---

## Installation

### From crates.io

```bash
pkg install rust clang gpgv termux-keyring
cargo install akg
```

Make sure `~/.cargo/bin` is in your `PATH`.

### From source

```bash
git clone https://github.com/lev73748/akg.git
cd akg
cargo build --release
cp target/release/akg $PREFIX/bin/
```

Requirements: Rust toolchain, `gpgv` and `termux-keyring` (for repository signature verification).

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
| `install`          | `i`, `add`       | Install packages, local `.deb` files or `.deb` links |
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
| `-r`, `--repo URL`      | Use custom repository instead of sources.list, repeatable. A link to a `.deb` installs that file |
| `--suite NAME`          | Repository suite (default: `stable`)             |
| `--comp NAME`           | Repository component (default: `main`)           |
| `--no-verify`           | Skip Release signature check (prints a warning)  |
| `--download-only`       | Only download `.deb` files                       |
| `--dry-run`             | Show the plan without making changes             |
| `--no-scripts`          | Do not run maintainer scripts                    |
| `--no-triggers`         | Do not run triggers                              |
| `--force-overwrite`     | Allow overwriting files of other packages        |
| `-V`, `--version`       | Print version                                    |
| `-h`, `--help`          | Show help                                        |

Options can also be written as `--opt=value`.

### Examples

```bash
# Update index
akg update

# Install packages
akg install neovim htop

# Install a specific version
akg install git=2.45.0

# Install local .deb (no download, no dependency resolution, like dpkg -i)
akg -f ./package.deb
akg install ./package.deb
akg install https://example.com/package.deb

# Use a custom mirror (https:// is added automatically)
akg -r mirror.yandex.ru/mirrors/termux/termux-main install fish

# Custom repository with its own suite and component
akg -r https://example.org/repo --suite stable --comp main update

# Only download packages
akg install --download-only neovim

# Upgrade everything
akg upgrade

# Remove package and unused dependencies
akg uninstall nodejs

# Search
akg search python
```

A local `.deb` is installed as is. akg only warns if its dependencies are missing. The package architecture must match the device (or be `all`).

### Progress output

When stderr is a terminal, akg shows a download bar (percent, speed, ETA, packages done, bytes), an unpack bar, and one-line steps for unpacking, configuring and triggers. At the end it prints the total time and the time of each phase:

```
akg: 5 package(s) processed in 12.4s
     fetch 8.1s · unpack 1.2s · configure 2.4s · triggers 0.3s
```

When output is not a terminal (pipe, log file), plain lines are printed instead of bars.

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

## Security

- The repository signature is required by default. If `gpgv` or the trusted keys are missing, or the repository is not signed, akg stops with an error. `--no-verify` turns the check off explicitly and prints a warning. When a repository sets `signed-by`, only those keys are used.
- Package indexes are checked against the signed `Release`, and every `.deb` is checked against the SHA-256 from the index. A cached `.deb` is reused only if its size and SHA-256 match. A package without a SHA-256 in the index is not installed.
- Paths inside a package are validated before anything is written: `..` is rejected and files can only be written inside `$PREFIX`.

Found a problem? Please open an issue.

## Triggers

akg runs dpkg-style triggers declared in `*.triggers` files (`interest` / `activate`) after the packages are configured. Triggers raised by a script calling `dpkg-trigger` are not processed yet.

---

## Building

```bash
cargo build --release
cargo test
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

- Параллельная загрузка и распаковка пакетов
- Прогресс-бары: скорость, оставшееся время, сколько пакетов готово, время каждого этапа
- Разрешение зависимостей (Depends, Pre-Depends, Conflicts, Breaks, Provides, Replaces)
- Установка локальных `.deb` (путь или URL), для файлов ничего не скачивается
- Свои репозитории через `--repo` (достаточно адреса, `sources.list` править не нужно)
- Проверка подписи репозитория и SHA-256 индексов и пакетов
- Maintainer-скрипты и триггеры в стиле dpkg
- Метки auto-installed (совместимо с apt)
- Атомарные обновления базы status
- Режимы dry-run и download-only
- Тестовый root через `AKG_ROOT`

### Установка из crates.io

```bash
pkg install rust clang gpgv termux-keyring
cargo install akg
```

Убедись, что `~/.cargo/bin` есть в `PATH`.

### Установка из исходников

```bash
git clone https://github.com/lev73748/akg.git
cd akg
cargo build --release
cp target/release/akg $PREFIX/bin/
```

Нужен toolchain Rust, `gpgv` и `termux-keyring` (для проверки подписей репозиториев).

### Основные команды

| Команда            | Алиасы           | Описание                             |
|--------------------|------------------|--------------------------------------|
| `update`           | `upd`            | Обновить индекс                      |
| `install`          | `i`, `add`       | Установить пакеты, локальные `.deb` или ссылки на `.deb` |
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

### Опции

| Опция                   | Описание                                         |
|-------------------------|--------------------------------------------------|
| `-y`, `--yes`           | Отвечать «да» на вопросы                         |
| `-j`, `--jobs N`        | Число параллельных загрузок                      |
| `-f`, `--file PATH`     | Установить локальный `.deb` (путь или URL), можно несколько раз |
| `-r`, `--repo URL`      | Использовать свой репозиторий вместо sources.list, можно несколько раз. Ссылка на `.deb` ставит этот файл |
| `--suite NAME`          | Suite репозитория (по умолчанию `stable`)        |
| `--comp NAME`           | Компонент репозитория (по умолчанию `main`)      |
| `--no-verify`           | Не проверять подпись Release (с предупреждением) |
| `--download-only`       | Только скачать `.deb`                            |
| `--dry-run`             | Показать план без изменений                      |
| `--no-scripts`          | Не запускать maintainer-скрипты                  |
| `--no-triggers`         | Не запускать триггеры                            |
| `--force-overwrite`     | Разрешить перезапись файлов других пакетов       |
| `-V`, `--version`       | Версия                                           |
| `-h`, `--help`          | Справка                                          |

Опции можно писать и как `--opt=value`.

### Примеры

```bash
akg update
akg install neovim htop
akg install git=2.45.0
akg -f ./package.deb
akg install https://example.com/package.deb
akg -r mirror.yandex.ru/mirrors/termux/termux-main install fish
akg install --download-only neovim
akg upgrade
akg uninstall nodejs
akg search python
```

Локальный `.deb` ставится как есть, без разрешения зависимостей. Если зависимостей не хватает, akg только предупредит. Архитектура пакета должна совпадать с устройством (или быть `all`).

### Прогресс

Если вывод идёт в терминал, akg показывает бар загрузки (проценты, скорость, оставшееся время, сколько пакетов готово), бар распаковки и однострочные шаги для распаковки, конфигурации и триггеров. В конце печатается общее время и время каждого этапа. Если вывод не в терминал (пайп, лог), печатаются обычные строки.

### Безопасность

- Подпись репозитория обязательна по умолчанию. Если нет `gpgv` или ключей, либо репозиторий не подписан, akg останавливается с ошибкой. `--no-verify` отключает проверку явно и печатает предупреждение. Если у репозитория задан `signed-by`, используются только эти ключи.
- Индексы проверяются по подписанному `Release`, а каждый `.deb` по SHA-256 из индекса. Пакет из кэша используется только если совпали размер и SHA-256. Пакет без SHA-256 в индексе не устанавливается.
- Пути внутри пакета проверяются до записи: `..` отклоняется, писать можно только внутри `$PREFIX`.

Нашёл проблему? Открой issue.

### Триггеры

akg запускает триггеры в стиле dpkg из файлов `*.triggers` (`interest` / `activate`) после настройки пакетов. Триггеры, которые скрипт вызывает через `dpkg-trigger`, пока не обрабатываются.

### Переменные окружения

| Переменная   | Описание                                         |
|--------------|--------------------------------------------------|
| `PREFIX`     | Префикс Termux (по умолчанию `/data/data/com.termux/files/usr`) |
| `AKG_ROOT`   | Корневой каталог для тестов                      |
| `AKG_ARCH`   | Принудительная архитектура                       |

### Лицензия

MIT. См. [LICENSE](LICENSE).
