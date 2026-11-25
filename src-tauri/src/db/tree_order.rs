//! Where other users' folders and sessions are shown among the user's own, in
//! `PostgreSQL` mode.
//!
//! A place of the tree, the top level or a folder, can hold the rows of
//! several owners: the top level lists every user's shared folders, and a
//! folder of the user's own can still hold another user's session from
//! before `shellstation_own_folder`. The tree is listed by `sort_order`, and
//! a reorder writes it for the user's own rows alone, since a row is its
//! owner's alone to change (docs/DESIGN.md §4.5.1). Another user's rows keep
//! the numbers their owner gave them, so where they came among the user's
//! own after a reorder was left to how the numbers of two owners happened to
//! fall.
//!
//! So the order a user gives a place is kept here, in the local database, as
//! the place's IDs in that order, and the tree is laid out by it: each of
//! another user's rows follows the nearest of the user's own rows that came
//! before it in that order and is still there, or starts the place where
//! none did. The user's own rows keep the order their numbers give them,
//! which the reorder wrote. What the order does not name, such as a folder
//! another user has shared since, comes where its number puts it, as before.
//!
//! The table is made here and not by a migration, as the highlight choices'
//! are (`session_highlights`): the migrations are shared with `PostgreSQL`.
//! In `SQLite` mode there is one user, every row is theirs, and none of this
//! is used.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::models::{Folder, Session};
use super::DbResult;

/// What a place lists, which is ordered apart: folders and sessions are
/// listed apart, the folders first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Session,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Folder => "folder",
            Kind::Session => "session",
        }
    }
}

/// Tauri managed state: the user's own orders in `PostgreSQL` mode, `None` in
/// `SQLite` mode.
pub struct TreeOrderState(pub Option<TreeOrder>);

/// The local database's table of the order each place was given last.
#[derive(Clone)]
pub struct TreeOrder {
    pool: SqlitePool,
}

/// The key of a place in the table: the folder's ID, or nothing for the top
/// level.
fn place_key(place: Option<Uuid>) -> String {
    place.map(|id| id.to_string()).unwrap_or_default()
}

impl TreeOrder {
    /// Use `pool`, the local database, and make the table if it is not there
    /// yet.
    pub async fn open(pool: SqlitePool) -> DbResult<Self> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS local_tree_order ( \
                 kind TEXT NOT NULL, \
                 place TEXT NOT NULL, \
                 ids TEXT NOT NULL, \
                 PRIMARY KEY (kind, place) \
             )",
        )
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to create the table of tree orders: {e}"))?;
        Ok(Self { pool })
    }

    /// Keep `order` as the order of what `place` lists of `kind`, in place of
    /// the one kept before. An empty order forgets it.
    pub async fn record(&self, kind: Kind, place: Option<Uuid>, order: &[Uuid]) -> DbResult<()> {
        let query = if order.is_empty() {
            sqlx::query("DELETE FROM local_tree_order WHERE kind = ? AND place = ?")
                .bind(kind.as_str())
                .bind(place_key(place))
        } else {
            let ids = order
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(",");
            sqlx::query(
                "INSERT INTO local_tree_order (kind, place, ids) VALUES (?, ?, ?) \
                 ON CONFLICT (kind, place) DO UPDATE SET ids = excluded.ids",
            )
            .bind(kind.as_str())
            .bind(place_key(place))
            .bind(ids)
        };
        query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to save the order of a place in the tree: {e}"))?;
        Ok(())
    }

    /// Forget the orders of `places`, folders that are gone.
    pub async fn forget(&self, places: &[Uuid]) -> DbResult<()> {
        if places.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;
        for place in places {
            sqlx::query("DELETE FROM local_tree_order WHERE place = ?")
                .bind(place.to_string())
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to forget the order of a place in the tree: {e}"))?;
        }
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    /// Every order kept for `kind`, by place. An ID that does not read as
    /// one is left out, and so is a place whose key does not.
    async fn all(&self, kind: Kind) -> DbResult<HashMap<Option<Uuid>, Vec<Uuid>>> {
        let rows = sqlx::query("SELECT place, ids FROM local_tree_order WHERE kind = ?")
            .bind(kind.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Failed to read the orders of places in the tree: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let place: String = row.get("place");
                let place = if place.is_empty() {
                    None
                } else {
                    Some(Uuid::parse_str(&place).ok()?)
                };
                let ids: String = row.get("ids");
                let ids = ids
                    .split(',')
                    .filter_map(|id| Uuid::parse_str(id).ok())
                    .collect();
                Some((place, ids))
            })
            .collect())
    }

    /// Lay `folders` out by the orders kept here (see the module). `role` is
    /// the role the app connected as; while it is not known, no row is the
    /// user's own and nothing is laid out. If the orders cannot be read, the
    /// folders stay as listed.
    pub async fn arrange_folders(&self, folders: &mut Vec<Folder>, role: Option<&str>) {
        let Some(role) = role else { return };
        match self.all(Kind::Folder).await {
            Ok(orders) => arrange(
                folders,
                |f| f.parent_id,
                |f| f.id,
                |f| f.owner == role,
                &orders,
            ),
            Err(e) => tracing::warn!("Folders are shown in the order of their numbers: {e:?}"),
        }
    }

    /// As [`Self::arrange_folders`], for sessions.
    pub async fn arrange_sessions(&self, sessions: &mut Vec<Session>, role: Option<&str>) {
        let Some(role) = role else { return };
        match self.all(Kind::Session).await {
            Ok(orders) => arrange(
                sessions,
                |s| Some(s.folder_id),
                |s| s.id,
                |s| s.owner == role,
                &orders,
            ),
            Err(e) => tracing::warn!("Sessions are shown in the order of their numbers: {e:?}"),
        }
    }
}

/// Lay `items` out by `orders`, the order kept for each place. `items` are
/// as listed, by their numbers; what each place lists keeps the slots it had
/// in the list, so that only the order within a place changes.
pub fn arrange<T, P, I>(
    items: &mut Vec<T>,
    place_of: impl Fn(&T) -> P,
    id_of: impl Fn(&T) -> I,
    own: impl Fn(&T) -> bool,
    orders: &HashMap<P, Vec<I>>,
) where
    P: Eq + Hash + Copy,
    I: Eq + Hash + Copy,
{
    if orders.is_empty() {
        return;
    }
    let mut by_place: HashMap<P, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        by_place.entry(place_of(item)).or_default().push(i);
    }
    // For each slot of the list, the item that goes there.
    let mut slot_item: Vec<usize> = (0..items.len()).collect();
    for (place, slots) in &by_place {
        let Some(order) = orders.get(place) else {
            continue;
        };
        let arranged = arrange_place(slots, items, &id_of, &own, order);
        for (slot, item) in slots.iter().zip(arranged) {
            slot_item[*slot] = item;
        }
    }
    let mut taken: Vec<Option<T>> = std::mem::take(items).into_iter().map(Some).collect();
    items.extend(slot_item.into_iter().filter_map(|i| taken[i].take()));
}

/// The items of one place, `listed` (indexes into `items`) in their listed
/// order, laid out by `order`.
fn arrange_place<T, I>(
    listed: &[usize],
    items: &[T],
    id_of: &impl Fn(&T) -> I,
    own: &impl Fn(&T) -> bool,
    order: &[I],
) -> Vec<usize>
where
    I: Eq + Hash + Copy,
{
    let at: HashMap<I, usize> = listed.iter().map(|&i| (id_of(&items[i]), i)).collect();
    // Another user's rows that the order names, by the row of the user's
    // own they follow (`None`: the start of the place), in the order's order.
    let mut follows: HashMap<Option<I>, Vec<usize>> = HashMap::new();
    let mut placed: HashSet<usize> = HashSet::new();
    let mut anchor: Option<I> = None;
    for id in order {
        let Some(&i) = at.get(id) else { continue };
        if own(&items[i]) {
            anchor = Some(*id);
        } else if placed.insert(i) {
            follows.entry(anchor).or_default().push(i);
        }
    }
    let mut arranged = Vec::with_capacity(listed.len());
    arranged.extend(follows.remove(&None).unwrap_or_default());
    for &i in listed {
        if placed.contains(&i) {
            continue;
        }
        arranged.push(i);
        if own(&items[i]) {
            arranged.extend(follows.remove(&Some(id_of(&items[i]))).unwrap_or_default());
        }
    }
    arranged
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;

    /// An item of a place: its ID, its place and whether it is the user's own.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Item {
        id: char,
        place: u8,
        own: bool,
    }

    fn mine(id: char) -> Item {
        Item {
            id,
            place: 0,
            own: true,
        }
    }

    fn theirs(id: char) -> Item {
        Item {
            id,
            place: 0,
            own: false,
        }
    }

    /// `items` laid out by the order `order` for place 0.
    fn laid_out(items: &[Item], order: &str) -> String {
        let mut items = items.to_vec();
        let orders = HashMap::from([(0u8, order.chars().collect::<Vec<_>>())]);
        arrange(&mut items, |i| i.place, |i| i.id, |i| i.own, &orders);
        items.iter().map(|i| i.id).collect()
    }

    #[test]
    fn the_order_asked_for_is_the_order_shown() {
        // The user's own rows are listed in the order the reorder numbered
        // them; another user's rows are listed wherever their numbers fall.
        let listed = [theirs('X'), mine('b'), theirs('Y'), mine('a'), mine('c')];
        assert_eq!(laid_out(&listed, "bXaYc"), "bXaYc");
        assert_eq!(laid_out(&listed, "XYbac"), "XYbac");
        assert_eq!(laid_out(&listed, "bacYX"), "bacYX");
    }

    #[test]
    fn a_row_that_is_gone_hands_its_followers_to_the_one_before() {
        // `b` has gone to another folder since: X followed it, and now
        // follows `a`, the nearest of the user's own before it.
        let listed = [mine('a'), mine('c'), theirs('X')];
        assert_eq!(laid_out(&listed, "abXc"), "aXc");
        // With no row of the user's own before it, it starts the place.
        assert_eq!(laid_out(&listed, "bXac"), "Xac");
    }

    #[test]
    fn what_the_order_does_not_name_comes_where_its_number_puts_it() {
        // Z was shared after the reorder, and `d` made: both as listed.
        let listed = [mine('a'), theirs('Z'), mine('b'), theirs('X'), mine('d')];
        assert_eq!(laid_out(&listed, "Xab"), "XaZbd");
    }

    #[test]
    fn the_users_own_rows_keep_the_order_of_their_numbers() {
        // Sorted since, on another machine: the user's own rows go by their
        // numbers, and another user's follow the row they followed.
        let listed = [mine('c'), mine('b'), mine('a'), theirs('X')];
        assert_eq!(laid_out(&listed, "aXbc"), "cbaX");
    }

    #[test]
    fn an_id_named_twice_counts_where_it_is_named_first() {
        let listed = [mine('b'), mine('a'), theirs('X')];
        assert_eq!(laid_out(&listed, "bXaX"), "bXa");
    }

    #[test]
    fn rows_of_another_place_in_the_order_count_for_nothing() {
        // `p` is the user's own and X another user's, both in place 1 now:
        // named in place 0's order, neither anchors nor is placed there.
        let mut items = vec![
            mine('a'),
            theirs('Y'),
            Item {
                id: 'p',
                place: 1,
                own: true,
            },
            Item {
                id: 'X',
                place: 1,
                own: false,
            },
        ];
        let orders = HashMap::from([(0u8, "pYXa".chars().collect::<Vec<_>>())]);
        arrange(&mut items, |i| i.place, |i| i.id, |i| i.own, &orders);
        let ids: String = items.iter().map(|i| i.id).collect();
        assert_eq!(ids, "YapX");
    }

    #[test]
    fn other_places_keep_their_slots() {
        let mut items = vec![
            Item {
                id: 'p',
                place: 1,
                own: true,
            },
            mine('a'),
            Item {
                id: 'q',
                place: 1,
                own: false,
            },
            theirs('X'),
        ];
        let orders = HashMap::from([(0u8, vec!['X', 'a'])]);
        arrange(&mut items, |i| i.place, |i| i.id, |i| i.own, &orders);
        let ids: String = items.iter().map(|i| i.id).collect();
        assert_eq!(ids, "pXqa");
    }

    async fn local() -> TreeOrder {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(":memory:"))
            .await
            .unwrap();
        TreeOrder::open(pool).await.unwrap()
    }

    fn folder(id: Uuid, parent_id: Option<Uuid>, owner: &str) -> Folder {
        Folder {
            id,
            name: "folder".to_string(),
            parent_id,
            sort_order: 0,
            owner: owner.to_string(),
            visibility: "shared".to_string(),
        }
    }

    #[tokio::test]
    async fn an_order_is_kept_and_replaced_and_forgotten() {
        let order = local().await;
        let parent = Uuid::new_v4();
        let (a, b, x) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let listed = vec![
            folder(a, Some(parent), "me"),
            folder(x, Some(parent), "them"),
            folder(b, Some(parent), "me"),
        ];
        let ids = |folders: &[Folder]| folders.iter().map(|f| f.id).collect::<Vec<_>>();

        order
            .record(Kind::Folder, Some(parent), &[x, a, b])
            .await
            .unwrap();
        // Another kind's order, and the top level's, are apart.
        order
            .record(Kind::Session, Some(parent), &[a])
            .await
            .unwrap();
        order.record(Kind::Folder, None, &[b, x]).await.unwrap();
        let mut folders = listed.clone();
        order.arrange_folders(&mut folders, Some("me")).await;
        assert_eq!(ids(&folders), [x, a, b]);

        // While the role is not known, nothing is laid out.
        let mut folders = listed.clone();
        order.arrange_folders(&mut folders, None).await;
        assert_eq!(ids(&folders), [a, x, b]);

        order
            .record(Kind::Folder, Some(parent), &[a, b, x])
            .await
            .unwrap();
        let mut folders = listed.clone();
        order.arrange_folders(&mut folders, Some("me")).await;
        assert_eq!(ids(&folders), [a, b, x]);

        order.forget(&[parent]).await.unwrap();
        let mut folders = listed.clone();
        order.arrange_folders(&mut folders, Some("me")).await;
        assert_eq!(ids(&folders), [a, x, b]);
        // The top level's order stays.
        assert_eq!(order.all(Kind::Folder).await.unwrap().len(), 1);

        order.record(Kind::Folder, None, &[]).await.unwrap();
        assert!(order.all(Kind::Folder).await.unwrap().is_empty());
    }

    fn session(id: Uuid, folder_id: Uuid, owner: &str) -> Session {
        Session {
            id,
            folder_id,
            name: "session".to_string(),
            hostname: "host.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "profile".to_string(),
            jump_host_id: None,
            tags: String::new(),
            icon: String::new(),
            sort_order: 0,
            highlight_profile_id: None,
            credential_profile_id: None,
            legacy_algorithms: false,
            login_sequence_id: None,
            owner: owner.to_string(),
            visibility: "shared".to_string(),
        }
    }

    #[tokio::test]
    async fn sessions_are_laid_out_by_their_own_orders() {
        let order = local().await;
        let folder = Uuid::new_v4();
        let (a, x) = (Uuid::new_v4(), Uuid::new_v4());
        let listed = vec![session(a, folder, "me"), session(x, folder, "them")];
        // A folder order of the same place is no session order.
        order
            .record(Kind::Folder, Some(folder), &[x, a])
            .await
            .unwrap();
        let mut sessions = listed.clone();
        order.arrange_sessions(&mut sessions, Some("me")).await;
        assert_eq!(sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [a, x]);

        order
            .record(Kind::Session, Some(folder), &[x, a])
            .await
            .unwrap();
        let mut sessions = listed.clone();
        order.arrange_sessions(&mut sessions, Some("me")).await;
        assert_eq!(sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [x, a]);
    }

    #[tokio::test]
    async fn what_does_not_read_as_an_id_is_left_out() {
        let order = local().await;
        let (a, x) = (Uuid::new_v4(), Uuid::new_v4());
        for (place, ids) in [
            (String::new(), format!("{x},not an id,,{a}")),
            ("not a folder".to_string(), format!("{a},{x}")),
        ] {
            sqlx::query("INSERT INTO local_tree_order (kind, place, ids) VALUES ('folder', ?, ?)")
                .bind(place)
                .bind(ids)
                .execute(&order.pool)
                .await
                .unwrap();
        }
        let all = order.all(Kind::Folder).await.unwrap();
        assert_eq!(all, HashMap::from([(None, vec![x, a])]));
    }
}
