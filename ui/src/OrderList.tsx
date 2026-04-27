import { Fragment, useEffect, useState } from 'react';
import { LAB_ORDER_PROFILE, LAB_ITEM_PROFILE, search } from './fhir';

type Order = {
  id: string;
  denpyoNo: string;
  patient: string;
  authoredOn: string;
  raw: any;
};

type Item = {
  id: string;
  orderId: string;
  codeDisplay: string;
  code: string;
  raw: any;
};

export function OrderList({ reloadKey }: { reloadKey: number }) {
  const [orders, setOrders] = useState<Order[]>([]);
  const [itemsByOrder, setItemsByOrder] = useState<Record<string, Item[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErr(null);
    try {
      const [orderBundle, itemBundle] = await Promise.all([
        search('ServiceRequest', { _profile: LAB_ORDER_PROFILE, _count: '100' }),
        search('ServiceRequest', { _profile: LAB_ITEM_PROFILE, _count: '500' }),
      ]);
      const loaded: Order[] = (orderBundle.entry ?? []).map((e) => {
        const r = e.resource;
        return {
          id: r.id,
          denpyoNo: r.identifier?.[0]?.value ?? '-',
          patient: r.subject?.reference ?? '-',
          authoredOn: r.authoredOn ?? r.meta?.lastUpdated ?? '-',
          raw: r,
        };
      });
      loaded.sort((a, b) => b.authoredOn.localeCompare(a.authoredOn));
      setOrders(loaded);

      // Group items by parent order via basedOn (client-side; based-on index not supported yet)
      const byOrder: Record<string, Item[]> = {};
      for (const e of itemBundle.entry ?? []) {
        const r = e.resource;
        const ref: string | undefined = r.basedOn?.[0]?.reference;
        if (!ref) continue;
        const orderId = ref.startsWith('ServiceRequest/') ? ref.slice('ServiceRequest/'.length) : ref;
        const coding = r.code?.coding?.[0];
        (byOrder[orderId] ??= []).push({
          id: r.id,
          orderId,
          codeDisplay: coding?.display ?? coding?.code ?? '-',
          code: coding?.code ?? '',
          raw: r,
        });
      }
      setItemsByOrder(byOrder);
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reloadKey]);

  const toggle = (id: string) => {
    setExpanded((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  return (
    <div className="card">
      <div className="toolbar">
        <div style={{ fontSize: 13, color: '#555' }}>
          LabOrder: {orders.length} 件
        </div>
        <button className="refresh" onClick={load} disabled={loading}>
          {loading ? '読み込み中...' : '再読み込み'}
        </button>
      </div>

      {err && <div className="msg-err">{err}</div>}

      <div className="query-urls">
        <div>
          <span className="method">GET</span>
          <code>/ServiceRequest?_profile={encodeURIComponent(LAB_ORDER_PROFILE)}&_count=100</code>
        </div>
        <div>
          <span className="method">GET</span>
          <code>/ServiceRequest?_profile={encodeURIComponent(LAB_ITEM_PROFILE)}&_count=500</code>
        </div>
      </div>

      <table>
        <thead>
          <tr>
            <th style={{ width: 40 }}></th>
            <th>伝票番号</th>
            <th>患者</th>
            <th>依頼日時</th>
            <th>項目数</th>
          </tr>
        </thead>
        <tbody>
          {orders.length === 0 && !loading && (
            <tr><td colSpan={5} style={{ textAlign: 'center', color: '#888', padding: 24 }}>依頼なし</td></tr>
          )}
          {orders.map((o) => {
            const items = itemsByOrder[o.id] ?? [];
            const isOpen = expanded.has(o.id);
            return (
              <Fragment key={o.id}>
                <tr className="order" onClick={() => toggle(o.id)}>
                  <td>{isOpen ? '▼' : '▶'}</td>
                  <td>{o.denpyoNo}</td>
                  <td>{o.patient}</td>
                  <td>{new Date(o.authoredOn).toLocaleString('ja-JP')}</td>
                  <td>{items.length}</td>
                </tr>
                {isOpen && (
                  <tr className="items">
                    <td colSpan={5}>
                      <div className="items-inner">
                        <details className="json-preview" open>
                          <summary>ServiceRequest/{o.id} (LabOrder)</summary>
                          <pre>{JSON.stringify(o.raw, null, 2)}</pre>
                        </details>
                        <div style={{ fontSize: 12, color: '#666', margin: '12px 0 4px' }}>
                          basedOn で紐づく LabItem ({items.length} 件)
                        </div>
                        {items.length === 0 && (
                          <div style={{ color: '#888', fontSize: 13 }}>項目なし</div>
                        )}
                        {items.map((it) => (
                          <details key={it.id} className="json-preview">
                            <summary>ServiceRequest/{it.id} — {it.codeDisplay} ({it.code})</summary>
                            <pre>{JSON.stringify(it.raw, null, 2)}</pre>
                          </details>
                        ))}
                      </div>
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
