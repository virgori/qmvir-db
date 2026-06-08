/**
 * QM SDK — JavaScript / TypeScript client for the QM Data Gateway.
 *
 * Usage:
 *   import { QMClient } from "./client";
 *   const qm = new QMClient("http://localhost:8400", { apiKey: "..." });
 *   const articles = await qm.find("articles", { where: { status: "published" } });
 */
export interface QMResponse<T = unknown> {
    ok: boolean;
    data?: T;
    error?: string;
    meta?: Record<string, unknown>;
}
export interface FindOptions {
    where?: Record<string, unknown>;
    select?: string[];
    orderBy?: Array<Record<string, "asc" | "desc">>;
    limit?: number;
    offset?: number;
}
export interface SearchOptions {
    filters?: Record<string, unknown>;
    strategy?: "lexical" | "vector" | "hybrid" | "rerank";
    limit?: number;
}
export interface AggregateOptions {
    groupBy: string[];
    metrics: Array<Record<string, string>>;
    where?: Record<string, unknown>;
}
export interface QMConfig {
    apiKey?: string;
    tenantId?: string;
}
export declare class QMClient {
    private baseUrl;
    private config;
    constructor(baseUrl?: string, config?: QMConfig);
    find<T = unknown>(entity: string, opts?: FindOptions): Promise<QMResponse<T[]>>;
    get<T = unknown>(entity: string, pk: string): Promise<QMResponse<T>>;
    insert<T = unknown>(entity: string, data: Record<string, unknown>): Promise<QMResponse<T>>;
    update<T = unknown>(entity: string, pk: string, data: Record<string, unknown>): Promise<QMResponse<T>>;
    delete(entity: string, pk: string): Promise<QMResponse<void>>;
    search<T = unknown>(collection: string, text: string, opts?: SearchOptions): Promise<QMResponse<T[]>>;
    aggregate<T = unknown>(dataset: string, opts: AggregateOptions): Promise<QMResponse<T[]>>;
    private request;
}
