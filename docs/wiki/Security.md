# Безопасность и границы доверия

## Git и provenance

Git хранит только текстовые конфиги и точные pins. Общая policy проверяется при export, PR, CI и deploy, включая tracked ignored файлы. SHA-256 гарантирует совпадение полученных байтов с pin, но не доказывает безопасность плагина или происхождение автора. Review источника и совместимости обязателен.

Секрет-детекция эвристическая. Она проверяет известные имена ключей, вложенные YAML/JSON/TOML значения и некоторые виды токенов. Она не является полной DLP-системой. Выбирайте файлы явно, читайте их, не экспортируйте папку plugins целиком. Нельзя объявлять userdata безопасными только потому, что они YAML.

Read-only deploy key не может пушить. Первичная настройка GitHub и `propose` используют отдельную локальную gh авторизацию разработчика. Не сохраняйте writable token на Minecraft машине ради агента. Workflows инструмента имеют contents:read; updater template получает write только в config-репозитории для feature branch/PR. Не используйте pull_request_target для исполнения непроверенного кода с write token.

## Сеть

Web и RCON client — loopback. **Minecraft RCON listener этим не ограничивается.** Закройте внешний TCP RCON на IPv4 и IPv6 до первого старта. Используйте SSH tunnel или отдельно проверенный HTTPS reverse proxy, не публикуйте встроенный HTTP UI напрямую. CLI transport допускает HTTP только для loopback; к удаленному endpoint нужен HTTPS.

Установщик не меняет firewall/SSH и не гарантирует безопасность сети автоматически. Сохраняйте резервный доступ. SSH host keys проверяйте независимо; не выключайте StrictHostKeyChecking.

## Права и процессы

Fresh MVP использует один UID для Java/агента/UI. Плагины считаются доверенным исполняемым кодом: они могут читать данные и секреты своего пользователя. Root-owned secrets.env 0600 предотвращает обычное чтение файла, но окружение процессов того же UID не является изоляцией от злонамеренного плагина. Разделение UID и управляющего socket — отдельное улучшение, не действующая гарантия.

Sudoers ограничивается конкретными systemctl start/stop/is-active выбранного unit. Изменение YAML service/path не выдает новые права; изменения sandbox/sudoers требуют отдельной проверки администратора. Не выдавайте wildcard systemctl/root shell.

## Перенос

Нет распаковки архивов. Запрещены traversal, symlinks и неподдерживаемые paths; есть лимиты chunk/file count/session/staging, свободного места и TTL. Scoped token открывает только конкретную сессию; храните session JSON вне Git. Согласованность source snapshot подтверждает оператор. Backend проверяет SHA и остановленный target перед заменой мира.

## Остаточные риски

Подключение игрока возможно между последним online-check и stop. Миграция данных не откатывается возвратом JAR. Внешняя БД требует собственного согласованного backup. Retention backup/cache/staging пока не автоматизирован; контролируйте диск. Tests с fixtures не заменяют стенд с вашей сборкой.

Local write-only provider принимает только rcon/web роли в dedicated adjacent 0600 file. Проверяются owner, permissions, symlink/hardlink, schema и размер; malformed store не замещается env молча. Preview содержит random generation marker, не password digest. Settings сохраняются с CAS и shared locks; active request нельзя очистить по одному TTL. Destination binding и journal защищают перенос при смене config.
