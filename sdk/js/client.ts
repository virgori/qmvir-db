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

const STRATEGY_MAP: Record<string, Record<string, boolean>> = {
  lexical: { lexical: true },
  vector: { vector: true },
  hybrid: { lexical: true, vector: true },
  rerank: { lexical: true, vector: true, rerank: true },
};

export class QMClient {
  private baseUrl: string;
  private config: QMConfig;

  constructor(baseUrl = "http://localhost:8400", config: QMConfig = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
    this.config = config;
  }

  async find<T = unknown>(entity: string, opts: FindOptions = {}): Promise<QMResponse<T[]>> {
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

  async get<T = unknown>(entity: string, pk: string): Promise<QMResponse<T>> {
    return this.request({ action: "get", entity, where: { id: pk }, limit: 1 });
  }

  async insert<T = unknown>(entity: string, data: Record<string, unknown>): Promise<QMResponse<T>> {
    return this.request({ action: "insert", entity, data });
  }

  async update<T = unknown>(entity: string, pk: string, data: Record<string, unknown>): Promise<QMResponse<T>> {
    return this.request({ action: "update", entity, where: { id: pk }, data });
  }

  async delete(entity: string, pk: string): Promise<QMResponse<void>> {
    return this.request({ action: "delete", entity, where: { id: pk } });
  }

  async search<T = unknown>(collection: string, text: string, opts: SearchOptions = {}): Promise<QMResponse<T[]>> {
    return this.request({
      action: "search",
      collection,
      text,
      filters: opts.filters,
      strategy: STRATEGY_MAP[opts.strategy ?? "lexical"],
      limit: opts.limit ?? 10,
    });
  }

  async aggregate<T = unknown>(dataset: string, opts: AggregateOptions): Promise<QMResponse<T[]>> {
    return this.request({
      action: "aggregate",
      dataset,
      group_by: opts.groupBy,
      metrics: opts.metrics,
      where: opts.where,
    });
  }

  private async request<T>(payload: Record<string, unknown>): Promise<QMResponse<T>> {
    try {
      const headers: Record<string, string> = { "Content-Type": "application/json" };
      if (this.config.apiKey) headers["Authorization"] = `Bearer ${this.config.apiKey}`;
      if (this.config.tenantId) headers["X-Tenant-ID"] = this.config.tenantId;

      const resp = await fetch(`${this.baseUrl}/query`, {
        method: "POST",
        headers,
        body: JSON.stringify(payload),
      });
      const body = await resp.json();
      return { ok: body.ok ?? false, data: body.data, error: body.error, meta: body.meta };
    } catch (err) {
      return { ok: false, error: String(err) };
    }
  }
}
