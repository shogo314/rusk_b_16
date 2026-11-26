# 数値・配列・整列コンテナと順列

以下はすべて標準で読み込まれます。署名の一覧は[APIリファレンス](api.md)で検索できます。動作例は[standard_library.rusk](https://github.com/shogo314/rusk_b_16/blob/main/examples/beta11/standard_library.rusk)にあります。

## 順列

```rusk
for row in permutations([2, 1, 1]) {
    wt row;
}
```

`1 1 2`、`1 2 1`、`2 1 1`の順に列挙します。`permutations(Vec<T>)`は入力をコピーして昇順に並べ替え、`Permutations<T>`という反復状態を返します。入力は変更しません。順序上同値の要素を区別せず、空配列の順列は1個です。各結果は要素ごとに`__copy__`した独立した配列です。全結果を先に保存せず、必要な順列だけ作ります。1回の取り出しはコピーと次の順列への更新にO(n)を要し、保持する配列はO(n)です。要素のコピーと比較にかかる時間は別途必要です。

通常の代入`let alias := it;`は列挙位置も共有します。`let independent = it;`または`copy(it)`で現在位置から独立した列挙を作れます。内部フィールドは直接変更しないでください。循環する要素のコピー・比較は利用者が実装します。

既存配列を更新する`next_permutation(values)`・`prev_permutation(values)`もあります。次があればtrue、末尾なら先頭に戻してfalseです。前方向は昇順へ、逆方向は降順へ戻ります。`next_permutation_by_comp`・`prev_permutation_by_comp`は比較関数を受け取ります。要素には全順序が必要で、比較中に入力を変更しないでください。

## BigInt

`BigInt("123456789012345678901234567890")`で任意精度整数を作ります。`Int`・`Int128`からも明示変換できます。巨大な整数リテラルを直接書く構文はありません。

四則演算・剰余・比較・累乗・ビット演算・シフト、`abs`・`min`・`max`・`gcd`・`lcm`、入出力・文字列変換・ハッシュに対応します。型間の暗黙変換はありません。`Int(value)`・`Int128(value)`は範囲外なら実行時エラーです。除算は負の無限大へ丸め、剰余は除数と同符号です。ゼロ除算と負の指数を検査します。負数のビット演算は無限の2の補数表現として扱います。右シフトは算術シフト、負のシフト量は逆向きです。

実装は32ビット桁の符号付き整数で、外部ライブラリへの依存はありません。乗算は通常の筆算、除算は2進の長除算です。巨大整数向けの高速乗算はまだありません。サイズは利用可能なメモリに制限されます。

## IntArray・BitVec・Bits

| 型 | 構築例 | 格納 |
|---|---|---|
| `IntArray` | `IntArray([3,1,2])`、`IntArray(5,0)` | 64ビット整数を連続格納 |
| `BitVec` | `BitVec("10101")`、`BitVec(5,false)` | 64ビット単位で圧縮格納 |
| `Bits<N>` | `Bits::<5>("10101")`、`Bits::<5>(false)` | 型で長さを区別する圧縮ビット列 |

これらは要素ごとの共有オブジェクトを保持しません。添字取得はreadonlyな独立値です。`a[i] = x`でその値を変更することはできません。元の配列は`a[i] := x`または`a.set(i,x)`で更新します。`a.get(i)`は変更可能な独立値を返します。走査も独立した値を返します。

`IntArray`は追加・末尾削除・resize・sort・reverse・sum・入出力に対応します。ビット列はcount・any・all・none、set・flip、ビット演算とシフトに対応します。文字列の右端が添字0です。シフトは長さを維持し、外へ出たビットを捨てます。長さが異なるビット列の二項演算、範囲外アクセス、負の長さは実行時エラーです。`N`はコンパイル時のInt値引数です。

## Deque・Array・Slice

`Deque<T>`は両端への追加・削除を行うコンテナです。`Deque([1,2])`または`Deque::<Int>()`で作り、`push_front`・`push_back`・`pop_front`・`pop_back`、`front`・`back`、添字・走査を使えます。

`Array<T,N>`は長さを型で区別する配列です。`Array::<Int,3>([1,2,3])`または`Array::<Int,3>(0)`で作ります。長さ違いは実行時エラーです。fill形式は各位置に初期値をコピーします。内部格納は動的配列で、長さを変える公開操作はありません。

両型の取得は要素を共有します。`a[i] = value`は既存の要素を更新し、`a[i] := value`はその位置の共有先を交換します。`copy`は各要素をコピーします。走査中に構造を変更すると実行時エラーになります。

`slice(owner,left,right)`は半開区間のビューです。所有元は`Vec`・`IntArray`・`Deque`・`Array`・別のSliceです。Vec・IntArray・Deque・Arrayでは、境界を省略すると全体を参照します。所有元を保持するため、元の変数がなくなっても使えます。要素の更新は共有し、所有元の構造変更後は使用時に失敗します。構造変更そのものを禁止する借用方式ではありません。

`readonly_slice(owner)`または`readonly_slice(slice(owner,left,right))`はreadonlyなビューを返します。ビューの`copy`は所有元全体をコピーして同じ区間を参照します。区間だけを独立した`Vec`にしたいときは`view.copy_to_vec()`を使います。これはreadonlyなビューにも使えます。文字列・ビット列のスライスは現在提供しません。内部のowner・範囲・versionは直接変更しないでください。

## 整列コンテナ { #ordered-containers }

| 型 | 用途 |
|---|---|
| `OrderedSet<K>` | 重複しないキーの集合 |
| `MultiSet<K>` | 重複を保持する集合 |
| `OrderedDict<K,V>` | キー順の辞書 |

引数なしで構築し、キーの全順序で走査します。`insert`または`set`、`contains`・`clear`、lower_bound・upper_bound・first・lastを提供します。境界取得はキーのコピーを`Option<K>`で返します。

`OrderedSet` と `OrderedDict` の削除には `erase` を使います。`MultiSet` の `erase_one(key)` は1件を削除し、削除できたかを `Bool` で返します。`erase_all(key)` は同値のキーをすべて削除し、削除した件数を `Int` で返します。`count(key)` で同値のキーの件数を取得します。

```rusk
let keys := MultiSet::<Int>();
keys.insert(2);
keys.insert(2);
wt keys.erase_one(2), keys.count(2); // Yes 1
wt keys.erase_all(2), keys.count(2); // 1 0
```

辞書のgetは`Option<V>`を返して値を共有します。添字取得は不在なら引数なしコンストラクタで値を登録します。走査ではキーをコピーし、値は共有します。keys・valuesはキー順のVecを返します。挿入するキーはコピーして保持するため、呼び出し元のキー変更では木の順序は変わりません。手動の`__copy__`がキーを共有する場合、そのキーを後で変更しない責任は利用者にあります。コピーはキー・値の双方をコピーします。

検索・挿入・単一削除・境界検索は通常O(log n)、走査とコピーはO(n)です。キーには`__cmp__`などの全順序が必要です。比較の実行中に同じコンテナへ再入する操作は実行時エラーです。独自の比較に副作用がある場合、readonlyなコンテナからの操作を保守的に拒否することがあります。

## その他のアルゴリズム

Vecにrotate・nth_element・partial_sort・unique・partitionを追加しました。uniqueは隣接する同値要素をまとめて配列を短くし、新しい長さを返します。partitionは条件に合う要素を前へ集め、区切り位置を返します。

ソート済みVecにはmerge_sorted・set_union・set_intersection・set_difference・set_symmetric_differenceを使えます。結果の外側は新しく作り、要素は入力と共有します。入力が同じ全順序で整列済みであることが前提です。

partial_sumは累積和、adjacent_differenceは隣接差、inner_productは内積です。gcd・lcmはInt・Int128・BigIntに対応します。固定幅整数では結果の範囲を検査します。
