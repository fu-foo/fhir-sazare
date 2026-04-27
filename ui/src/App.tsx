import { useState } from 'react';
import { CreateOrder } from './CreateOrder';
import { OrderList } from './OrderList';

type Tab = 'create' | 'list';

export function App() {
  const [tab, setTab] = useState<Tab>('create');
  const [listReloadKey, setListReloadKey] = useState(0);

  return (
    <div className="container">
      <h1>ServiceRequest 検査依頼デモ</h1>
      <div className="subtitle">
        伝票(ServiceRequest_LabOrder) と 項目(ServiceRequest_LabItem) を別プロファイルとして登録し、
        <code> _profile </code> で検索します。
      </div>
      <nav>
        <button
          className={tab === 'create' ? 'active' : ''}
          onClick={() => setTab('create')}
        >依頼作成</button>
        <button
          className={tab === 'list' ? 'active' : ''}
          onClick={() => setTab('list')}
        >依頼一覧</button>
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
