"use strict";
/**
 * QM SDK — JavaScript / TypeScript client for the QM Data Gateway.
 *
 * Usage:
 *   import { QMClient } from "./client";
 *   const qm = new QMClient("http://localhost:8400", { apiKey: "..." });
 *   const articles = await qm.find("articles", { where: { status: "published" } });
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.QMClient = void 0;
const STRATEGY_MAP = {
    lexical: { lexical: true },
    vector: { vector: true },
    hybrid: { lexical: true, vector: true },
    rerank: { lexical: true, vector: true, rerank: true },
};
class QMClient {
    constructor(baseUrl = "http://localhost:8400", config = {}) {
        this.baseUrl = baseUrl.replace(/\/+$/, "");
        this.config = config;
    }
    async find(entity, opts = {}) {
        return this.request({
            action: "find",
            entity,
            where: opts.where,
            select: opts.select,
            order_by: opts.orderBy,
            limit: opts.limit ?? 20,
            offset: opts.offset ?? 0,
        });
    }
    async get(entity, pk) {
        return this.request({ action: "get", entity, where: { id: pk }, limit: 1 });
    }
    async insert(entity, data) {
        return this.request({ action: "insert", entity, data });
    }
    async update(entity, pk, data) {
        return this.request({ action: "update", entity, where: { id: pk }, data });
    }
    async delete(entity, pk) {
        return this.request({ action: "delete", entity, where: { id: pk } });
    }
    async search(collection, text, opts = {}) {
        return this.request({
            action: "search",
            collection,
            text,
            filters: opts.filters,
            strategy: STRATEGY_MAP[opts.strategy ?? "lexical"],
            limit: opts.limit ?? 10,
        });
    }
    async aggregate(dataset, opts) {
        return this.request({
            action: "aggregate",
            dataset,
            group_by: opts.groupBy,
            metrics: opts.metrics,
            where: opts.where,
        });
    }
    async request(payload) {
        try {
            const headers = { "Content-Type": "application/json" };
            if (this.config.apiKey)
                headers["Authorization"] = `Bearer ${this.config.apiKey}`;
            if (this.config.tenantId)
                headers["X-Tenant-ID"] = this.config.tenantId;
            const resp = await fetch(`${this.baseUrl}/query`, {
                method: "POST",
                headers,
                body: JSON.stringify(payload),
            });
            const body = await resp.json();
            return { ok: body.ok ?? false, data: body.data, error: body.error, meta: body.meta };
        }
        catch (err) {
            return { ok: false, error: String(err) };
        }
    }
}
exports.QMClient = QMClient;
