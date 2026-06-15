import { useMemo, useState } from 'react';
import { TEST_ITEMS, TestItem, buildOrderBundle, postBundle } from './fhir';

const autoDenpyo = () => {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  return `DEN-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
};

export function CreateOrder({ onCreated }: { onCreated: () => void }) {
  const [patientId, setPatientId] = useState('patient-demo-001');
  const [denpyoNo, setDenpyoNo] = useState('');
  const [selected, setSelected] = useState<Set<string>>(new Set(['3A015000002327101']));
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null);

  const toggle = (code: string) => {
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(code)) next.delete(code); else next.add(code);
      return next;
    });
  };

  const submit = async () => {
    setMsg(null);
    if (!patientId.trim()) { setMsg({ kind: 'err', text: 'Please enter a Patient ID' }); return; }
    const items: TestItem[] = TEST_ITEMS.filter((t) => selected.has(t.code));
    if (items.length === 0) { setMsg({ kind: 'err', text: 'Please select at least one test item' }); return; }
    setBusy(true);
    try {
      const bundle = buildOrderBundle({
        patientId: patientId.trim(),
        denpyoNo: denpyoNo.trim() || autoDenpyo(),
        items,
      });
      const res = await postBundle(bundle);
      const created = res.entry?.length ?? 0;
      setMsg({ kind: 'ok', text: `Created: ${created} resources (1 order + ${created - 1} items)` });
      setDenpyoNo('');
      onCreated();
    } catch (e) {
      setMsg({ kind: 'err', text: (e as Error).message });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card">
      <label>Patient ID</label>
      <input
        type="text"
        value={patientId}
        onChange={(e) => setPatientId(e.target.value)}
        placeholder="patient-demo-001"
      />

      <label>Order No. (auto-generated if blank)</label>
      <input
        type="text"
        value={denpyoNo}
        onChange={(e) => setDenpyoNo(e.target.value)}
        placeholder="DEN-..."
      />

      <label>Test Items ({selected.size} selected)</label>
      <div className="check-grid">
        {TEST_ITEMS.map((t) => (
          <label key={t.code}>
            <input
              type="checkbox"
              checked={selected.has(t.code)}
              onChange={() => toggle(t.code)}
            />
            {t.display}
          </label>
        ))}
      </div>

      <button className="primary" onClick={submit} disabled={busy}>
        {busy ? 'Submitting...' : 'Submit Order'}
      </button>

      {msg?.kind === 'ok' && <div className="msg-ok">{msg.text}</div>}
      {msg?.kind === 'err' && <div className="msg-err">{msg.text}</div>}

      <BundlePreview
        patientId={patientId}
        denpyoNo={denpyoNo}
        items={TEST_ITEMS.filter((t) => selected.has(t.code))}
      />
    </div>
  );
}

function BundlePreview({ patientId, denpyoNo, items }: {
  patientId: string; denpyoNo: string; items: TestItem[];
}) {
  const preview = useMemo(() => {
    if (!patientId.trim() || items.length === 0) return null;
    return buildOrderBundle({
      patientId: patientId.trim(),
      denpyoNo: denpyoNo.trim() || 'DEN-(auto)',
      items,
    });
  }, [patientId, denpyoNo, items]);

  return (
    <details className="json-preview" open>
      <summary>Bundle JSON to be sent (transaction)</summary>
      {preview
        ? <pre>{JSON.stringify(preview, null, 2)}</pre>
        : <div style={{ color: '#888', fontSize: 13, padding: 8 }}>Enter a Patient ID and test items to see a preview</div>}
    </details>
  );
}
