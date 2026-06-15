import { useState } from 'react';
import { CreateOrder } from './CreateOrder';
import { OrderList } from './OrderList';

type Tab = 'create' | 'list';

export function App() {
  const [tab, setTab] = useState<Tab>('create');
  const [listReloadKey, setListReloadKey] = useState(0);

  return (
    <div className="container">
      <h1>ServiceRequest Lab Order Demo</h1>
      <div className="subtitle">
        Register the order (ServiceRequest_LabOrder) and its items (ServiceRequest_LabItem) as separate profiles,
        then search them with <code> _profile </code>.
      </div>
      <nav>
        <button
          className={tab === 'create' ? 'active' : ''}
          onClick={() => setTab('create')}
        >New Order</button>
        <button
          className={tab === 'list' ? 'active' : ''}
          onClick={() => setTab('list')}
        >Orders</button>
      </nav>
      {tab === 'create' && (
        <CreateOrder onCreated={() => {
          setListReloadKey((k) => k + 1);
          setTab('list');
        }} />
      )}
      {tab === 'list' && <OrderList reloadKey={listReloadKey} />}
    </div>
  );
}
