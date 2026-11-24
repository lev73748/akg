# akg
                                                        [![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Termux-green.svg)](https://termux.dev/)
[![Version](https://img.shields.io/badge/version-0.5.8-blue.svg)](https://github.com/lev73748/akg)

Fast package manager for Termux written in Rust.
Compatible with the apt/dpkg package format and repository layout.

---

## Features

- Parallel package downloads and unpacking
- Progress bars for install **and** remove/purge: speed, ETA, packages done, time of every phase
- Dependency resolution (Depends, Pre-Depends, Conflicts, Breaks, Provides, Replaces)
- Local `.deb` installation (file path or URL)
- Custom repositories via `--repo` (no need to edit `sources.list`)
- Repository signature check and SHA-256 verification of indexes and packages
- Maintainer scripts and dpkg-style triggers
- Auto-installed marks (apt-compatible)
- Atomic status database updates
- `fix-broken`: finish interrupted installs, reinstall half-installed packages, pull in missing dependencies
- `search` and `list` mark installed packages
- Dry-run and download-only modes
- Optional root via `AKG_ROOT`
- Mirror benchmarking (`akg bench`) with latency + speed ranking
- Easy repository management (`add-repo`, `repo-choice`)

---

## Installation

### From TUR (recommended)

```bash
pkg install tur-repo
pkg install akg
```

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

Run without a command and without `-f`, akg prints the help and exits with code 1.
`akg -f <file.deb>` on its own is the same as `akg install -f <file.deb>`.

### Commands

| Command          | Aliases                                  | Description                                      |
|------------------|------------------------------------------|--------------------------------------------------|
| `update`         | `upd`                                    | Refresh package index                            |
| `install`        | `i`, `add`                               | Install packages, local `.deb` files or `.deb` URLs |
| `reinstall`      | `rei`                                    | Reinstall packages                               |
| `upgrade`        | `up`, `upg`, `full-upgrade`              | Refresh the index and upgrade all packages       |
| `uninstall`      | `remove`, `rm`, `un`, `del`              | Remove packages + unused dependencies            |
| `purge`          |                                          | Remove including config files                    |
| `autoremove`     |                                          | Remove unused dependencies                       |
| `fix-broken`     | `fix`, `repair`                          | Repair half-installed / unconfigured packages and missing dependencies |
| `search`         | `se`                                     | Search packages (installed ones are marked)      |
| `show`           | `sh`, `info`, `inf`                      | Show package details                             |
| `files`          | `f`                                      | List files of an installed package               |
| `list`           | `list-all`                               | List all available packages (installed ones are marked) |
| `list-installed` | `list-i`                                 | List installed packages                          |
| `clean`          |                                          | Delete cached `.deb` files                       |
| `bench`          |                                          | Benchmark mirrors (ping + speed) and set default |
| `add-repo`       | `addrepo`                                | Add a custom repository set                      |
| `repo-choice`    | `repo-choise`, `choose-repo`             | Choose default repository interactively          |
| `help`           |                                          | Show help                                        |

### Options

| Option                | Used by                                  | Description                                                                 |
|-----------------------|------------------------------------------|-----------------------------------------------------------------------------|
| `-y`, `--yes`         | everything that asks                     | Assume yes. With `bench` it picks the fastest mirror, with `add-repo` it makes the new set the default |
| `-j`, `--jobs N`      | install, reinstall, upgrade              | Number of parallel downloads (default: depends on the CPU)                  |
| `-f`, `--file PATH`   | install                                  | Install local `.deb` (path or URL), repeatable                              |
| `-r`, `--repo URL`    | all except `add-repo`, `repo-choice`     | Use custom repository instead of sources.list, repeatable. A link to a `.deb` installs that file |
| `--suite NAME`        | with `-r`                                | Repository suite (default: `stable`)                                        |
| `--comp NAME`         | with `-r`                                | Repository component (default: `main`)                                      |
| `--no-verify`         | every Release check                      | Skip Release signature check (prints a warning)                             |
| `--download-only`     | install, reinstall, upgrade              | Only download `.deb` files                                                  |
| `--dry-run`           | install, reinstall, upgrade, fix-broken  | Show the plan without making changes                                        |
| `--no-scripts`        | install, reinstall, upgrade, fix-broken  | Do not run maintainer scripts                                               |
| `--no-triggers`       | install, reinstall, upgrade              | Do not run triggers                                                         |
| `--force-overwrite`   | install, reinstall, upgrade              | Allow overwriting files of other packages                                   |
| `-V`, `--version`     |                                          | Print version                                                               |
| `-h`, `--help`        |                                          | Show help                                                                   |

Options can also be written as `--opt=value`.

Maintainer scripts are never run when `AKG_ROOT` is set.

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

# Show what an upgrade would do
akg upgrade --dry-run

# Upgrade everything
akg upgrade

# Remove package and unused dependencies
akg uninstall nodejs

# Purge (remove + config files)
akg purge nodejs

# Search (installed packages get an [installed] mark)
akg search python

# List everything available / everything installed
akg list
akg list-installed

# Repair a broken installation
akg fix-broken --dry-run
akg fix-broken

# Benchmark mirrors and pick one (Enter = the fastest)
akg bench

# Benchmark and switch to the fastest mirror without asking
akg bench -y

# Add a custom repository (asks whether to make it the default, -y answers yes)
akg add-repo https://example.org/repo
akg add-repo -y https://example.org/repo

# Interactively choose default repository
akg repo-choice
```

A local `.deb` is installed as-is. akg only warns if its dependencies are missing. The package architecture must match the device (or be `all`).

### Search and list

`akg search <query>` matches package names and descriptions; exact and prefix matches come first. Several words must all match.

```
python/stable 3.12.8 [installed]
  Python 3 programming language

python-pip/stable 25.0 [installed: 24.3.1]
  A tool for installing and managing Python packages
```

`[installed]` means the same version is installed; `[installed: X]` means version `X` is installed and the repository has a different one. `akg list` prints one line per package: `name/stable version arch [installed]`. `search` exits with code 1 when nothing was found.

### fix-broken

`akg fix-broken` (aliases `fix`, `repair`) repairs what an interrupted or failed install leaves behind:

- packages in the `unpacked` or `half-configured` state: `postinst configure` is run again, dependencies first
- packages in the `half-installed` state: reinstalled from the repository
- installed packages with unmet dependencies: the missing dependencies are installed

Use `--dry-run` to only print what is broken. Reinstalling and installing dependencies needs the network; configuring does not. Exit code is 1 if something is still broken afterwards.

### Repositories

`akg bench` pings all mirrors from `etc/termux/mirrors`, measures the download speed of the package index on a limited number of threads, prints a table and asks for a number (Enter = the fastest). The chosen mirror is written as the default together with TUR. Run it only when you want it: `update` and `install` never call it.

`akg add-repo` takes one or several links, or a file with links:

```bash
akg add-repo https://example.org/termux-main                        # one link
akg add-repo https://a.example/main https://a.example/root          # several links = one set
akg add-repo "https://example.org/repo mysuite mycomp"              # link + suite + component
akg add-repo mirrors.txt                                            # file with sets
```

In a file, `[name]` starts a new set, one link per line, `#` is a comment. Links without a header form one set. Every link is checked against its signature before anything is written (`--no-verify` is NOT recommended here). When only a `termux-main` / `main` link is given, the `root` and `x11` repositories next to it are added if they exist. The sets are stored in `$PREFIX/etc/akg/repos.list`.

`akg repo-choice` shows all known sets (yours and the ones from `etc/termux/mirrors`) as a numbered list, marks the current one, and applies the one you pick. You can also paste a link instead of a number. It needs a terminal. `add-repo` and `repo-choice` cannot be combined with `--repo`.

When a set becomes the default, akg writes `sources.list` and `sources.list.d/*.list` (the first change of each file keeps a copy as `*.akg-bak`), removes the indexes of the old mirror and refreshes the new ones.

### Progress output

When stderr is a terminal, akg shows:

- **Install**: download bar (percent, speed, ETA, packages done), extract bar, and one-line steps for Install / Configure / Trigger
- **Remove / Purge**: progress bar for Remove or Purge phase

At the end it prints the total time and the time of each phase:

```
akg: 5 package(s) processed in 12.4s
     fetch 8.1s · unpack 1.2s · configure 2.4s · triggers 0.3s
```

```
akg: 3 package(s) processed in 1.8s
     remove 1.8s
```

When output is not a terminal (pipe, log file), plain lines are printed instead of bars.

### Environment

| Variable   | Description                                                        |
|------------|--------------------------------------------------------------------|
| `PREFIX`   | Termux prefix (default: `/data/data/com.termux/files/usr`)         |
| `AKG_ROOT` | Optional root for testing (e.g. `$HOME/fakeroot`)                  |
| `AKG_ARCH` | Force architecture                                                 |

Test mode example:

```bash
AKG_ROOT=$HOME/fakeroot akg install -y --no-scripts <pkg>
```

---

## Security

- The repository signature is required by default. If `gpgv` or the trusted keys are missing, or the repository is not signed, akg stops with an error. `--no-verify` turns the check off explicitly and prints a warning. When a repository sets `signed-by`, only those keys are used.
- Package indexes are checked against the signed `Release`, and every `.deb` is checked against the SHA-256 from the index. A cached `.deb` is reused only if its size and SHA-256 match. A package without a SHA-256 in the index is not installed.
- A `.deb` given by a direct link (`-f URL`, `install URL`) is not covered by any repository signature: akg warns about it, and only `https://` links are accepted.
- Paths inside a package are validated before anything is written: `..` is rejected and files can only be written inside `$PREFIX`.

Found a problem? Please open an issue.

---

## Triggers

akg runs dpkg-style triggers declared in `*.triggers` files (`interest` / `activate`) after the packages are configured. Triggers raised by a script calling `dpkg-trigger` are not processed yet. `fix-broken` does not run triggers.

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
- Прогресс-бары для установки **и** удаления/purge: скорость, оставшееся время, сколько пакетов готово, время каждого этапа
- Разрешение зависимостей (Depends, Pre-Depends, Conflicts, Breaks, Provides, Replaces)
- Установка локальных `.deb` (путь или URL)
- Свои репозитории через `--repo` (достаточно адреса, `sources.list` править не нужно)
- Проверка подписи репозитория и SHA-256 индексов и пакетов
- Maintainer-скрипты и триггеры в стиле dpkg
- Метки auto-installed (совместимо с apt)
- Атомарные обновления базы status
- `fix-broken`: довести до конца прерванную установку, переустановить недоустановленные пакеты, поставить недостающие зависимости
- `search` и `list` помечают установленные пакеты
- Режимы dry-run и download-only
- Тестовый root через `AKG_ROOT`
- Бенчмарк зеркал (`akg bench`) — пинг + скорость, выбор самого быстрого
- Удобное управление репозиториями (`add-repo`, `repo-choice`)

### Установка через TUR (рекомендуется)

```bash
pkg install tur-repo
pkg install akg
```

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

### Команды

Без команды и без `-f` akg печатает справку и завершается с кодом 1. `akg -f <file.deb>` без команды равно `akg install -f <file.deb>`.

| Команда          | Алиасы                                   | Описание                                             |
|------------------|------------------------------------------|------------------------------------------------------|
| `update`         | `upd`                                    | Обновить индекс                                      |
| `install`        | `i`, `add`                               | Установить пакеты, локальные `.deb` или ссылки на `.deb` |
| `reinstall`      | `rei`                                    | Переустановить                                       |
| `upgrade`        | `up`, `upg`, `full-upgrade`              | Обновить индекс и все пакеты                         |
| `uninstall`      | `remove`, `rm`, `un`, `del`              | Удалить + неиспользуемые зависимости                 |
| `purge`          |                                          | Удалить вместе с конфигами                           |
| `autoremove`     |                                          | Удалить неиспользуемые зависимости                   |
| `fix-broken`     | `fix`, `repair`                          | Починить недоустановленные / ненастроенные пакеты и недостающие зависимости |
| `search`         | `se`                                     | Поиск пакетов (установленные помечаются)             |
| `show`           | `sh`, `info`, `inf`                      | Информация о пакете                                  |
| `files`          | `f`                                      | Список файлов установленного пакета                  |
| `list`           | `list-all`                               | Все доступные пакеты (установленные помечаются)      |
| `list-installed` | `list-i`                                 | Список установленных пакетов                         |
| `clean`          |                                          | Очистить кэш `.deb`                                  |
| `bench`          |                                          | Бенчмарк зеркал (пинг + скорость) и выбор основного  |
| `add-repo`       | `addrepo`                                | Добавить свой набор репозиториев                     |
| `repo-choice`    | `repo-choise`, `choose-repo`             | Интерактивно выбрать основной репозиторий            |
| `help`           |                                          | Справка                                              |

### Опции

| Опция                 | Где работает                             | Описание                                                                      |
|-----------------------|------------------------------------------|-------------------------------------------------------------------------------|
| `-y`, `--yes`         | всё, что задаёт вопросы                  | Отвечать «да». С `bench` берёт самое быстрое зеркало, с `add-repo` сразу делает набор основным |
| `-j`, `--jobs N`      | install, reinstall, upgrade              | Число параллельных загрузок                                                   |
| `-f`, `--file PATH`   | install                                  | Установить локальный `.deb` (путь или URL), можно несколько раз               |
| `-r`, `--repo URL`    | всё, кроме `add-repo`, `repo-choice`     | Использовать свой репозиторий вместо sources.list, можно несколько раз. Ссылка на `.deb` ставит этот файл |
| `--suite NAME`        | вместе с `-r`                            | Suite репозитория (по умолчанию `stable`)                                     |
| `--comp NAME`         | вместе с `-r`                            | Компонент репозитория (по умолчанию `main`)                                   |
| `--no-verify`         | любая проверка Release                   | Не проверять подпись Release (с предупреждением)                              |
| `--download-only`     | install, reinstall, upgrade              | Только скачать `.deb`                                                         |
| `--dry-run`           | install, reinstall, upgrade, fix-broken  | Показать план без изменений                                                   |
| `--no-scripts`        | install, reinstall, upgrade, fix-broken  | Не запускать maintainer-скрипты                                               |
| `--no-triggers`       | install, reinstall, upgrade              | Не запускать триггеры                                                         |
| `--force-overwrite`   | install, reinstall, upgrade              | Разрешить перезапись файлов других пакетов                                    |
| `-V`, `--version`     |                                          | Версия                                                                        |
| `-h`, `--help`        |                                          | Справка                                                                       |

Опции можно писать и как `--opt=value`. Если задан `AKG_ROOT`, maintainer-скрипты не запускаются никогда.

### Примеры

```bash
akg update
akg install neovim htop
akg install git=2.45.0
akg -f ./package.deb
akg install https://example.com/package.deb
akg -r mirror.yandex.ru/mirrors/termux/termux-main install fish
akg install --download-only neovim
akg upgrade --dry-run
akg upgrade
akg uninstall nodejs
akg purge nodejs
akg search python
akg list
akg list-installed
akg fix-broken --dry-run
akg fix-broken
akg bench
akg bench -y
akg add-repo https://example.org/repo
akg add-repo -y https://example.org/repo
akg repo-choice
```

Локальный `.deb` ставится как есть, без разрешения зависимостей. Если зависимостей не хватает, akg только предупредит. Архитектура пакета должна совпадать с устройством (или быть `all`).

### Поиск и список

`akg search <запрос>` ищет по именам и описаниям, точные совпадения и совпадения по началу имени идут первыми. Если слов несколько, должны совпасть все. `[installed]` значит, что стоит такая же версия, `[installed: X]` значит, что стоит версия `X`, а в репозитории другая. `akg list` печатает по строке на пакет: `имя/stable версия архитектура [installed]`. `search` возвращает код 1, если ничего не найдено.

### fix-broken

`akg fix-broken` (алиасы `fix`, `repair`) чинит то, что остаётся после прерванной или неудачной установки:

- пакеты в состоянии `unpacked` или `half-configured`: `postinst configure` запускается заново, сначала зависимости
- пакеты в состоянии `half-installed`: переустанавливаются из репозитория
- установленные пакеты с невыполненными зависимостями: недостающие зависимости устанавливаются

С `--dry-run` только печатает, что сломано. Для переустановки и установки зависимостей нужна сеть, для настройки не нужна. Если после работы что-то осталось сломанным, код возврата 1. Триггеры `fix-broken` не запускает.

### Репозитории

`akg bench` пингует все зеркала из `etc/termux/mirrors`, мерит скорость загрузки индекса пакетов на ограниченном числе потоков, печатает таблицу и просит номер (Enter = самое быстрое). Выбранное зеркало записывается как основное вместе с TUR. Запускается только вручную, `update` и `install` его не вызывают.

`akg add-repo` принимает одну или несколько ссылок либо файл со ссылками:

```bash
akg add-repo https://example.org/termux-main                        # одна ссылка
akg add-repo https://a.example/main https://a.example/root          # несколько ссылок = один набор
akg add-repo "https://example.org/repo mysuite mycomp"              # ссылка + suite + компонент
akg add-repo mirrors.txt                                            # файл с наборами
```

В файле `[имя]` начинает новый набор, одна ссылка на строку, `#` — комментарий. Ссылки без заголовка образуют один набор. Подпись каждой ссылки проверяется до записи (`--no-verify` здесь использовать не стоит). Если дана только ссылка на `termux-main` / `main`, то `root` и `x11` рядом добавляются, когда они существуют. Наборы хранятся в `$PREFIX/etc/akg/repos.list`.

`akg repo-choice` показывает все известные наборы (свои и из `etc/termux/mirrors`) нумерованным списком, отмечает текущий и применяет выбранный. Вместо номера можно вставить ссылку. Нужен терминал. `add-repo` и `repo-choice` нельзя сочетать с `--repo`.

Когда набор становится основным, akg пишет `sources.list` и `sources.list.d/*.list` (при первом изменении файла остаётся копия `*.akg-bak`), удаляет индексы старого зеркала и обновляет новые.

### Прогресс

`akg search <запрос>` ищет по именам и описаниям, точные совпадения и совпадения по началу имени идут первыми. Если слов несколько, должны совпасть все. `[installed]` значит, что стоит такая же версия, `[installed: X]` значит, что стоит версия `X`, а в репозитории другая. `akg list` печатает по строке на пакет: `имя/stable версия архитектура [installed]`. `search` возвращает код 1, если ничего не найдено.

### fix-broken

`akg fix-broken` (алиасы `fix`, `repair`) чинит то, что остаётся после прерванной или неудачной установки:

- пакеты в состоянии `unpacked` или `half-configured`: `postinst configure` запускается заново, сначала зависимости
- пакеты в состоянии `half-installed`: переустанавливаются из репозитория
- установленные пакеты с невыполненными зависимостями: недостающие зависимости устанавливаются

С `--dry-run` только печатает, что сломано. Для переустановки и установки зависимостей нужна сеть, для настройки не нужна. Если после работы что-то осталось сломанным, код возврата 1. Триггеры `fix-broken` не запускает.
- Подпись репозитория обязательна по умолчанию. Если нет `gpgv` или ключей, либо репозиторий не подписан, akg останавливается с ошибкой. `--no-verify` отключает проверку явно и печатает предупреждение. Если у репозитория задан `signed-by`, используются только эти ключи.
- Индексы проверяются по подписанному `Release`, а каждый `.deb` по SHA-256 из индекса. Пакет из кэша используется только если совпали размер и SHA-256. Пакет без SHA-256 в индексе не устанавливается.
- `.deb` по прямой ссылке (`-f URL`, `install URL`) не покрыт подписью репозитория: akg предупреждает об этом и принимает только `https://`.
- Пути внутри пакета проверяются до записи: `..` отклоняется, писать можно только внутри `$PREFIX`.

### Репозитории

`akg bench` пингует все зеркала из `etc/termux/mirrors`, мерит скорость загрузки индекса пакетов на ограниченном числе потоков, печатает таблицу и просит номер (Enter = самое быстрое). Выбранное зеркало записывается как основное вместе с TUR. Запускается только вручную, `update` и `install` его не вызывают.

`akg add-repo` принимает одну или несколько ссылок либо файл со ссылками:

```bash
akg add-repo https://example.org/termux-main                        # одна ссылка
akg add-repo https://a.example/main https://a.example/root          # несколько ссылок = один набор
akg add-repo "https://example.org/repo mysuite mycomp"              # ссылка + suite + компонент
akg add-repo mirrors.txt                                            # файл с наборами
```

В файле `[имя]` начинает новый набор, одна ссылка на строку, `#` — комментарий. Ссылки без заголовка образуют один набор. Подпись каждой ссылки проверяется до записи (`--no-verify` здесь использовать не стоит). Если дана только ссылка на `termux-main` / `main`, то `root` и `x11` рядом добавляются, когда они существуют. Наборы хранятся в `$PREFIX/etc/akg/repos.list`.

`akg repo-choice` показывает все известные наборы (свои и из `etc/termux/mirrors`) нумерованным списком, отмечает текущий и применяет выбранный. Вместо номера можно вставить ссылку. Нужен терминал. `add-repo` и `repo-choice` нельзя сочетать с `--repo`.

Когда набор становится основным, akg пишет `sources.list` и `sources.list.d/*.list` (при первом изменении файла остаётся копия `*.akg-bak`), удаляет индексы старого зеркала и обновляет новые.
                                                           ### Прогресс
                                                           Если вывод идёт в терминал, akg показывает:                                                                           - **Установка**: бар загрузки (проценты, скорость, ETA, сколько пакетов готово), бар распаковки и шаги Install / Configure / Trigger                                             - **Удаление / purge**: прогресс-бар фазы Remove или Purge                                                            В конце печатается общее время и время каждого этапа. Если вывод не в терминал (пайп, лог), печатаются обычные строки.
                                                           ### Безопасность                                                                                                      - Подпись репозитория обязательна по умолчанию. Если нет `gpgv` или ключей, либо репозиторий не подписан, akg останавливается с ошибкой. `--no-verify` отключает проверку явно и печатает предупреждение. Если у репозитория задан `signed-by`, используются только эти ключи.                         - Индексы проверяются по подписанному `Release`, а каждый `.deb` по SHA-256 из индекса. Пакет из кэша используется только если совпали размер и SHA-256. Пакет без SHA-256 в индексе не устанавливается.                                    - `.deb` по прямой ссылке (`-f URL`, `install URL`) не покрыт подписью репозитория: akg предупреждает об этом и принимает только `https://`.                                     - Пути внутри пакета проверяются до записи: `..` отклоняется, писать можно только внутри `$PREFIX`.                                                                              Нашёл проблему? Открой issue.
                                                           ### Триггеры
                                                           akg запускает триггеры в стиле dpkg из файлов `*.triggers` (`interest` / `activate`) после настройки пакетов. Триггеры, которые скрипт вызывает через `dpkg-trigger`, пока не обрабатываются.                                                                                                          ### Переменные окружения
                                                           | Переменная | Описание                                                        |
|------------|-----------------------------------------------------------------|                                      | `PREFIX`   | Префикс Termux (по умолчанию `/data/data/com.termux/files/usr`) |                                      | `AKG_ROOT` | Корневой каталог для тестов                                     |
| `AKG_ARCH` | Принудительная архитектура                                      |                                      
### Лицензия                                                                                                          MIT. См. [LICENSE](LICENSE).
