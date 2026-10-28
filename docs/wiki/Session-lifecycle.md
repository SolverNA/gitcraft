# Сессии переноса, отмена и очистка

На странице `/transfer` есть список сессий: status, expiry, staged/reserved bytes и объяснение, почему очистка разрешена или защищена. Admin видит metadata, а не scoped tokens других сессий. Файлы идут напрямую клиент→сервер; подтвержденные offsets и chunk hashes позволяют продолжить после reload/обрыва, повторно выбрав тот же источник.

## Отмена без удаления

Кнопка «Отменить текущую сессию» использует scoped token. Admin API также может отменить сессию по ID. Отменяется только idle receiving/verified сессия без незавершенного world journal; байты еще не удаляются. Active network upload держит тот же session lock на протяжении чтения body, поэтому TTL не разрешает очистить работающий запрос.

## Preview → review → cleanup

Выберите IDs (пустой список — все допустимые) и покажите план очистки. В нем есть candidates, protected reasons и reclaim bytes. После проверки отметьте review и примените **тот же digest**. Изменившаяся сессия требует нового плана. Completed/cancelled/expired idle sessions могут быть удалены; active или recovery-required с world journal защищены независимо от TTL.

Удаление использует durable cleanup journal и tombstones. При обрыве повторите apply с тем же digest; операция проверяет текущее состояние и продолжает безопасно. Imported JAR cache/receipts не удаляются: session receipt архивируется отдельно в state/transfer-receipts, и pins продолжают загружаться из проверенного кеша.

Orphan/unreadable session metadata выводится как protected, а не silently deleted. Автоматической безусловной retention для миров, backup и cache нет. Завершенные staged bytes по-прежнему считаются в quota до явной очистки.

## CLI

```sh
gitcraft transfer list --config /etc/gitcraft/config/agent.yml
gitcraft transfer cancel --endpoint http://127.0.0.1:8765 --session /private/session.json
gitcraft transfer cleanup-plan --config /etc/gitcraft/config/agent.yml --id SESSION_ID
gitcraft transfer cleanup-apply --config /etc/gitcraft/config/agent.yml --digest PLAN_DIGEST --reviewed
```

Локальные list/cleanup команды используют права service пользователя на state. Endpoint cancel использует scoped session file. Admin HTTP lifecycle routes требуют admin token или действующую browser session + CSRF; один scoped token не дает права перечислять/удалять чужие сессии.
