# Архитектура

## Три независимых потока

| Поток | Где работает | Результат |
|---|---|---|
| Настройки | Export → config Git → PR → merge | Точный commit выбранной deploy-ветки |
| Исполняемые файлы | HTTPS download или прямой transport | SHA-проверенный кеш с receipt для локального переноса |
| Игровые данные | Прямой transport и отдельные backup | Миры и локальные базы, никогда Git |

Репозиторий инструмента хранит Rust-код, установщик, systemd, UI и шаблоны. Репозиторий сервера хранит только желаемое состояние конфигов и указатели зависимостей.

## Обновление

Бот в Actions проверяет стабильные совместимые релизы поддерживаемого источника и открывает PR с новыми pins. Агент не выбирает версии сам. Он получает configured branch, фиксирует SHA, выполняет общий policy audit, готовит staging, проверяет JAR, проверяет RCON и период отсутствия игроков. Он останавливает Minecraft только после успешной подготовки.

Управляемые конфиги копируются по явным mappings, удаляются только ранее управляемые файлы. Миры и runtime-данные не синхронизируются с Git. Локальный RCON secret накладывается отдельно.

## Mappings

| Репозиторий | Рабочий сервер |
|---|---|
| `server-settings/server.properties` | `server.properties` |
| `server-settings/spigot.yml` | `spigot.yml` |
| `plugins-configs/Example/config.yml` | `plugins/Example/config.yml` |
| `config/example.toml` | `config/example.toml` |
| `defaultconfigs/example.toml` | `defaultconfigs/example.toml` |
| Engine pin filename | Указанный top-level `.jar` |
| Plugin/mod pin filename | `plugins/` или `mods/` |

Моды с отдельными libraries, argfiles и сложной установкой не превращаются автоматически в поддерживаемую сборку одним JAR. Existing unit сохраняется adopt-путем; его launch contract должен соответствовать выбранным pins.

## Отказоустойчивость

`deploy.lock` защищает от параллельного deploy/adopt/recovery/изменения runtime через transport. Durable journal фиксирует стадии применения и backup hashes. После старта новой Java возможны миграции игровых данных: ошибка readiness оставляет сервер остановленным и требует осознанного восстановления, а не молчаливого downgrade.

Работающий RCON недоступен — это ошибка, а не нулевой онлайн. Период пустого сервера уменьшает вероятность случайного рестарта при переподключении, но не блокирует новый вход между последней проверкой и остановкой.

## Проверяемый launcher и явная инициализация

Перед любым deploy, включая unchanged commit и fresh bootstrap, unit проверяется по структурированным `busctl --json=short`/systemd ExecStartEx данным: точный WorkingDirectory, один direct Java launch и единственный literal `-jar` target, совпадающий с engine pin. Переменные (`$JAVA_OPTS`), wrappers, несколько команд, `@argfiles`, неподдерживаемые flags и неоднозначный argv отвергаются до stop/apply. Нужен поддерживаемый system bus/ExecStartEx; текстовый вывод не считается безопасным fallback.

Fresh template использует explicit `ExecStart=/usr/bin/java -Xms1G -Xmx2G -jar server.jar nogui`. Старые env-based units не переписываются автоматически: владелец отдельно проверяет и мигрирует launcher при системной установке/обслуживании. Панель выбирает уже установленный service, не создает arbitrary unit.

Без verified initial state обычный agent возвращает initialization_required: выберите явный EULA bootstrap либо reviewed adopt. Пустой RCON не разрешает присвоить чужой runtime или начать первый deploy.
