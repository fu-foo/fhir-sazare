export const LAB_ORDER_PROFILE = 'http://example.org/fhir/StructureDefinition/ServiceRequest_LabOrder';
export const LAB_ITEM_PROFILE = 'http://example.org/fhir/StructureDefinition/ServiceRequest_LabItem';

export type TestItem = { code: string; display: string };

export const TEST_ITEMS: TestItem[] = [
  { code: '3A015000002327101', display: 'WBC count' },
  { code: '3A025000002327101', display: 'RBC count' },
  { code: '3B035000002327101', display: 'Blood glucose' },
  { code: '3D010000002327101', display: 'HbA1c' },
  { code: '3F015000002327101', display: 'AST' },
  { code: '3F020000002327101', display: 'ALT' },
];

const LAB_SYSTEM = 'urn:oid:1.2.392.200119.4.504';

async function fhir<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    credentials: 'same-origin',
    ...init,
    headers: {
      'Content-Type': 'application/fhir+json',
      Accept: 'application/fhir+json',
      ...(init?.headers as Record<string, string> | undefined),
    },
  });
  if (!res.ok) {
    throw new Error(`${init?.method ?? 'GET'} ${path} → ${res.status}: ${await res.text()}`);
  }
  return res.json() as Promise<T>;
}

export type Bundle = {
  resourceType: 'Bundle';
  type: string;
  total?: number;
  entry?: Array<{
    resource: any;
    response?: { status: string; location?: string };
    search?: { mode?: 'match' | 'include' };
  }>;
};

export async function search(type: string, params: Record<string, string>): Promise<Bundle> {
  const qs = new URLSearchParams(params).toString();
  return fhir<Bundle>(`/${type}?${qs}`);
}

export async function postBundle(bundle: Bundle): Promise<Bundle> {
  return fhir<Bundle>('/', { method: 'POST', body: JSON.stringify(bundle) });
}

export function buildOrderBundle(args: {
  patientId: string;
  denpyoNo: string;
  items: TestItem[];
}): Bundle {
  const orderFullUrl = `urn:uuid:${crypto.randomUUID()}`;
  const now = new Date().toISOString();

  const order = {
    resourceType: 'ServiceRequest',
    meta: { profile: [LAB_ORDER_PROFILE] },
    identifier: [{ system: 'http://example.org/denpyo', value: args.denpyoNo }],
    status: 'active',
    intent: 'order',
    category: [
      { coding: [{ system: 'http://snomed.info/sct', code: '108252007', display: 'Laboratory procedure' }] },
    ],
    subject: { reference: `Patient/${args.patientId}` },
    authoredOn: now,
  };

  const itemEntries = args.items.map((it) => ({
    fullUrl: `urn:uuid:${crypto.randomUUID()}`,
    resource: {
      resourceType: 'ServiceRequest',
      meta: { profile: [LAB_ITEM_PROFILE] },
      basedOn: [{ reference: orderFullUrl }],
      status: 'active',
      intent: 'order',
      code: { coding: [{ system: LAB_SYSTEM, code: it.code, display: it.display }] },
      subject: { reference: `Patient/${args.patientId}` },
    },
    request: { method: 'POST', url: 'ServiceRequest' },
  }));

  return {
    resourceType: 'Bundle',
    type: 'transaction',
    entry: [
      {
        fullUrl: orderFullUrl,
        resource: order,
        request: { method: 'POST', url: 'ServiceRequest' },
      } as any,
      ...itemEntries,
    ],
  };
}
