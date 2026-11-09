package app;

import java.util.*;
import java.util.function.*;
import java.util.stream.*;

public class Repo {
    interface Identifiable<ID extends Comparable<ID>> { ID getId(); }
    interface Repository<T extends Identifiable<ID>, ID extends Comparable<ID>> {
        Optional<T> findById(ID id);
        List<T> findAll();
        T save(T entity);
        default Map<ID, T> index() {
            return findAll().stream().collect(Collectors.toMap(Identifiable::getId, Function.identity()));
        }
        static <X extends Identifiable<Y>, Y extends Comparable<Y>> Repository<X, Y> inMemory() {
            return new MemoryRepository<>();
        }
    }
    static class MemoryRepository<T extends Identifiable<ID>, ID extends Comparable<ID>> implements Repository<T, ID> {
        private final Map<ID, T> store = new TreeMap<>();
        public Optional<T> findById(ID id) { return Optional.ofNullable(store.get(id)); }
        public List<T> findAll() { return new ArrayList<>(store.values()); }
        public T save(T e) { store.put(e.getId(), e); return e; }
    }
    static abstract class Base<S extends Base<S>> {
        String name;
        @SuppressWarnings("unchecked")
        S self() { return (S) this; }
        S named(String n) { this.name = n; return self(); }
    }
    static class User extends Base<User> implements Identifiable<Long> {
        long id; int age;
        public Long getId() { return id; }
        User age(int a) { this.age = a; return this; }
    }
    enum Op implements IntBinaryOperator {
        ADD("+") { public int applyAsInt(int a, int b) { return a + b; } },
        MUL("*") { public int applyAsInt(int a, int b) { return a * b; } };
        final String sym;
        Op(String s) { sym = s; }
    }
    static class Outer {
        int v = 1;
        class Inner { int twice() { return v * 2; } }
        Inner make() { return new Inner(); }
        static <T, R> List<R> mapAll(Collection<? extends T> in, Function<? super T, ? extends R> f) {
            List<R> out = new ArrayList<>();
            for (T t : in) out.add(f.apply(t));
            return out;
        }
    }
    static void print(Object o) {}
    static void print(String s) {}
    static void print(int i) {}
    static void print(Object... os) {}
    @SafeVarargs static <T> List<T> listOf(T... xs) { return Arrays.asList(xs); }

    void run() {
        Repository<User, Long> repo = Repository.inMemory();
        User u = new User().named("bob").age(3);
        repo.save(u);
        Optional<User> found = repo.findById(1L);
        String n = found.map(x -> x.name).orElse("none");
        Map<Long, User> idx = repo.index();
        Map<Boolean, List<User>> parts = repo.findAll().stream().collect(Collectors.partitioningBy(x -> x.age > 2));
        Map<String, Long> counts = repo.findAll().stream().collect(Collectors.groupingBy(x -> x.name, Collectors.counting()));
        int total = Op.ADD.applyAsInt(1, 2) + Op.valueOf("MUL").applyAsInt(2, 3);
        String sym = Op.MUL.sym;
        Outer o = new Outer();
        Outer.Inner in = o.new Inner();
        int t = in.twice() + o.make().twice();
        List<Integer> lens = Outer.mapAll(Arrays.asList("a", "bb"), String::length);
        int first = lens.get(0);
        print("s"); print(1); print(2.0); print("a", "b");
        List<String> strs = listOf("x", "y");
        Comparator<User> cmp = Comparator.comparing((User x) -> x.name).thenComparingInt(x -> x.age);
        repo.findAll().sort(cmp.reversed());
        Runnable r = new Runnable() { int count; public void run() { count++; System.out.println(count); } };
        r.run();
        BiFunction<Integer, Integer, Integer> add = Integer::sum;
        Supplier<List<String>> sup = ArrayList::new;
        sup.get().add("z");
        StringBuilder sb = new StringBuilder().append(1).append("a").reverse();
        char c = sb.charAt(0);
        int[] arr = {3, 1, 2};
        Arrays.sort(arr);
        int mx = Arrays.stream(arr).max().getAsInt();
        String joined = strs.stream().map(String::toUpperCase).collect(Collectors.joining(","));
        double avg = repo.findAll().stream().mapToInt(x -> x.age).average().orElse(0);
    }
}
