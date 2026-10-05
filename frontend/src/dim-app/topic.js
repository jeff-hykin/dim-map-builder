// Topic checking shared by pages (zenoh.js) and servers (frontend_publish.js, backend_state.js), kept apart from zenoh.js
// so a backend that publishes never pulls in the browser's zenoh-web client.

/** `<topic…>` chunks the relay accepts (letters, digits, `-`, `_`, `.`); `*` / `**` allowed for subscribing. */
export function checkTopic(topic, { wildcards = false } = {}) {
    const chunk = wildcards ? /^([A-Za-z0-9_.-]+|\*|\*\*)$/ : /^[A-Za-z0-9_.-]+$/
    if (typeof topic !== "string" || !topic.split("/").every((part) => chunk.test(part))) {
        throw new TypeError(`dim-app: bad topic ${JSON.stringify(topic)} (chunks of letters, digits, - _ .)`)
    }
    return topic
}
